//! 轻量级请求时序埋点模块
//!
//! 设计原则：
//! - 环形缓冲，定长（默认 2000 条），溢出自动覆盖最旧记录
//! - parking_lot::Mutex 保护，写入耗时 < 1µs，不阻塞正常请求
//! - Dashboard 聚合在读取端完成，不占用写入路径
//!
//! 典型用法：
//! ```
//! // 1. 创建全局实例（main.rs 里 Arc 注入）
//! let metrics = Arc::new(MetricsStore::new(2000));
//!
//! // 2. 每次请求完成后埋点（handler/middleware 里）
//! metrics.record(RequestEvent { ... });
//!
//! // 3. Dashboard endpoint 聚合
//! let overview = metrics.overview();
//! ```

use std::collections::VecDeque;
use std::sync::Arc;
use parking_lot::Mutex;
use serde::Serialize;

/// 单次请求埋点
#[derive(Debug, Clone)]
pub struct RequestEvent {
    /// Unix timestamp（秒）
    pub ts: i64,
    /// 凭据 ID（None = 鉴权失败/无凭据）
    pub cred_id: Option<u64>,
    /// 模型名（用于 TopN 统计）
    pub model: String,
    /// 请求耗时 ms
    pub latency_ms: u64,
    /// 输入 token 数
    pub input_tokens: u64,
    /// 输出 token 数
    pub output_tokens: u64,
    /// 是否成功（非 4xx/5xx）
    pub success: bool,
}

/// 环形请求事件缓冲
pub struct MetricsStore {
    buf: Mutex<VecDeque<RequestEvent>>,
    cap: usize,
}

impl MetricsStore {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: Mutex::new(VecDeque::with_capacity(cap.min(16_000))),
            cap: cap.max(100),
        }
    }

    /// 记录一次请求事件（O(1)）
    pub fn record(&self, ev: RequestEvent) {
        let mut buf = self.buf.lock();
        if buf.len() >= self.cap {
            buf.pop_front();
        }
        buf.push_back(ev);
    }

    /// 快照当前缓冲（克隆，后续聚合在无锁副本上完成）
    fn snapshot(&self) -> Vec<RequestEvent> {
        self.buf.lock().iter().cloned().collect()
    }

    // ----------------------------------------------------------------
    // 聚合 API（Dashboard 使用）
    // ----------------------------------------------------------------

    /// 近期请求总览（last 1h / 24h 双窗口 + 模型 TopN）
    pub fn overview(&self, now_ts: i64) -> DashboardOverview {
        let events = self.snapshot();

        let hour = now_ts - 3600;
        let day  = now_ts - 86400;

        let mut h_total = 0u64; let mut h_ok = 0u64;
        let mut d_total = 0u64; let mut d_ok = 0u64;
        let mut h_latency_sum = 0u64; let mut h_latency_cnt = 0u64;

        let mut model_counts: std::collections::HashMap<String, u64> = Default::default();

        for ev in &events {
            if ev.ts >= day {
                d_total += 1;
                if ev.success { d_ok += 1; }
            }
            if ev.ts >= hour {
                h_total += 1;
                if ev.success { h_ok += 1; }
                h_latency_sum += ev.latency_ms;
                h_latency_cnt += 1;
                *model_counts.entry(ev.model.clone()).or_default() += 1;
            }
        }

        let mut model_top: Vec<ModelStat> = model_counts
            .into_iter()
            .map(|(model, count)| ModelStat { model, count })
            .collect();
        model_top.sort_by(|a, b| b.count.cmp(&a.count));
        model_top.truncate(10);

        DashboardOverview {
            requests_1h: h_total,
            requests_24h: d_total,
            success_rate_1h: if h_total > 0 { h_ok as f64 / h_total as f64 } else { 1.0 },
            success_rate_24h: if d_total > 0 { d_ok as f64 / d_total as f64 } else { 1.0 },
            avg_latency_ms_1h: if h_latency_cnt > 0 { h_latency_sum / h_latency_cnt } else { 0 },
            model_top,
        }
    }

    /// 返回最近 N 分钟按 interval_minutes 分桶的时序数据（折线图用）
    pub fn series(
        &self,
        now_ts: i64,
        window_minutes: u64,
        interval_minutes: u64,
    ) -> Vec<SeriesBucket> {
        let events = self.snapshot();
        let interval = (interval_minutes.max(1) * 60) as i64;
        let start_ts = now_ts - (window_minutes.max(1) * 60) as i64;
        let bucket_count = (window_minutes / interval_minutes.max(1)).max(1) as usize;

        let mut buckets: Vec<SeriesBucket> = (0..bucket_count)
            .map(|i| SeriesBucket {
                ts: start_ts + (i as i64 * interval),
                requests: 0,
                errors: 0,
                avg_latency_ms: 0,
            })
            .collect();

        let mut latency_acc: Vec<(u64, u64)> = vec![(0, 0); bucket_count]; // (sum, cnt)

        for ev in &events {
            if ev.ts < start_ts { continue; }
            let idx = ((ev.ts - start_ts) / interval) as usize;
            if idx >= bucket_count { continue; }
            buckets[idx].requests += 1;
            if !ev.success { buckets[idx].errors += 1; }
            latency_acc[idx].0 += ev.latency_ms;
            latency_acc[idx].1 += 1;
        }

        for (i, b) in buckets.iter_mut().enumerate() {
            if latency_acc[i].1 > 0 {
                b.avg_latency_ms = latency_acc[i].0 / latency_acc[i].1;
            }
        }

        buckets
    }
}

// ----------------------------------------------------------------
// 响应 DTO（直接 Serialize → JSON）
// ----------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardOverview {
    pub requests_1h: u64,
    pub requests_24h: u64,
    /// 0.0 – 1.0
    pub success_rate_1h: f64,
    pub success_rate_24h: f64,
    pub avg_latency_ms_1h: u64,
    pub model_top: Vec<ModelStat>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStat {
    pub model: String,
    pub count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesBucket {
    /// 桶起始 Unix ts（秒）
    pub ts: i64,
    pub requests: u64,
    pub errors: u64,
    pub avg_latency_ms: u64,
}

/// 全局单例 Arc，main.rs 创建后注入 AppState 和 AdminService
pub type SharedMetrics = Arc<MetricsStore>;
