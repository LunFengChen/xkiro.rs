//! Admin API 业务逻辑服务

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use anyhow::Context;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::anthropic::middleware::PromptCacheRuntime;
use crate::common::utf8::floor_char_boundary;
use crate::http_client::ProxyConfig;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::provider::KiroProvider;
use crate::kiro::token_manager::{LOW_BALANCE_THRESHOLD, MultiTokenManager};
use crate::model::config::{CompressionConfig, SystemPromptPosition, UserPreset};
use crate::model::runtime::SharedPromptConfig;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::error::AdminServiceError;
use super::types::{
    AddCredentialRequest, AddCredentialResponse, BalanceResponse, BatchRefreshBalanceResponse,
    BatchRefreshBalanceResultItem, BatchRefreshResponse, BatchRefreshResultItem,
    CachedBalanceItem, CachedBalancesResponse, CompressionConfigResponse, CredentialStatusItem,
    CredentialsStatusResponse, DisableBatchResponse, ExportKamItem, ExportTokenJsonItem,
    GlobalConfigResponse,
    ImportAction, ImportItemResult, ImportJobSnapshot, ImportJobStatus, ImportSummary,
    ImportTokenJsonRequest, ImportTokenJsonResponse,
    PresetItem,
    ProxyAutoAssignRequest, ProxyAutoAssignResponse, ProxyConfigResponse, ProxyImportRequest,
    ProxyImportResponse, ProxyTestResponse, ProxyUpsertRequest, RuntimeBalanceSnapshot,
    RuntimeStatsItem, RuntimeStatsResponse, StartImportJobResponse,
    SystemPromptResponse, TokenJsonItem, UpdateCompressionConfigRequest,
    UpdateGlobalConfigRequest, UpdateProxyConfigRequest, UpdateSystemPromptRequest,
    UpsertUserPresetRequest,
};
use crate::kiro::token_manager::CachedBalanceInfo;

/// 余额缓存过期时间（秒），5 分钟
const BALANCE_CACHE_TTL_SECS: i64 = 300;

#[derive(Default)]
struct RefreshBalanceStats {
    success: usize,
    failed: usize,
    low_balance_disabled: usize,
}

/// 计算超额额度剩余：仅当 overage_status=ENABLED 时 > 0
fn overage_remaining(balance: &BalanceResponse) -> f64 {
    if balance.overage_status.as_deref() == Some("ENABLED") {
        let used = (balance.current_usage - balance.usage_limit).max(0.0);
        (balance.overage_cap - used).max(0.0)
    } else {
        0.0
    }
}

/// 缓存的余额条目（含时间戳）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBalance {
    /// 缓存时间（Unix 秒）
    cached_at: f64,
    /// 缓存的余额数据
    data: BalanceResponse,
}

/// 缓存的模型列表条目（仅进程内）
#[derive(Debug, Clone)]
struct CachedModels {
    cached_at: std::time::Instant,
    data: crate::kiro::models::ListAvailableModelsResponse,
}

const MODELS_CACHE_TTL_SECS: u64 = 30 * 60;

/// Admin 服务
///
/// 封装所有 Admin API 的业务逻辑
pub struct AdminService {
    token_manager: Arc<MultiTokenManager>,
    /// Kiro Provider 引用，用于 region/endpoint/global_proxy 热更新双层同步
    kiro_provider: Option<Arc<KiroProvider>>,
    /// 共享压缩配置，与 AppState 同源（运行时热更新）
    compression_config: Arc<RwLock<CompressionConfig>>,
    /// Prompt Cache 运行时（共享引用，支持 ttl/accounting 热更新）
    prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
    /// 系统提示注入运行时（共享引用，支持热更新）
    prompt_runtime: SharedPromptConfig,
    /// 截断恢复识别开关（与 AppState 共享，admin 写入即热生效）
    truncation_recovery_notice: Arc<std::sync::atomic::AtomicBool>,
    balance_cache: Mutex<HashMap<u64, CachedBalance>>,
    cache_path: Option<PathBuf>,
    /// 已注册的端点名称集合（用于 add_credential 校验）
    known_endpoints: HashSet<String>,
    /// 余额查询并发限流（单条 + 批量共享同一信号量）
    ///
    /// 单条 `fetch_balance` 与批量 `force_refresh_balances_batch` 都从这里获取
    /// 许可，确保系统级别上对单凭据池的余额上游调用并发不会失控。
    /// 容量 8 与历史批量 Semaphore 等价。
    balance_semaphore: Arc<Semaphore>,
    /// 模型列表缓存（仅内存，TTL 30 分钟；按 (id, provider) 区分）
    models_cache: Mutex<HashMap<(u64, Option<String>), CachedModels>>,
    /// 代理池(引用式绑定的运行时来源)。admin 管理 CRUD/测试/分配。
    proxy_manager: Arc<crate::kiro::proxy_manager::ProxyManager>,
    /// 后台导入任务状态表 (job_id → snapshot)
    import_jobs: Arc<Mutex<HashMap<String, ImportJobSnapshot>>>,
    /// 共享多 API Key 列表（与鉴权中间件同源，写入即热生效）
    api_keys: crate::anthropic::middleware::SharedApiKeys,
    /// config.json 路径（持久化 api_keys 增删）
    config_path: Option<PathBuf>,
    /// 请求时序埋点
    pub metrics: crate::admin::metrics::SharedMetrics,
}

impl AdminService {
    pub fn new(
        token_manager: Arc<MultiTokenManager>,
        kiro_provider: Option<Arc<KiroProvider>>,
        compression_config: Arc<RwLock<CompressionConfig>>,
        prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
        prompt_runtime: SharedPromptConfig,
        truncation_recovery_notice: Arc<std::sync::atomic::AtomicBool>,
        known_endpoints: impl IntoIterator<Item = String>,
        proxy_manager: Arc<crate::kiro::proxy_manager::ProxyManager>,
        api_keys: crate::anthropic::middleware::SharedApiKeys,
        config_path: Option<PathBuf>,
        metrics: crate::admin::metrics::SharedMetrics,
    ) -> Self {
        let cache_path = token_manager
            .cache_dir()
            .map(|d| d.join("kiro_balance_cache.json"));

        let balance_cache = Self::load_balance_cache_from(&cache_path);

        Self {
            token_manager,
            kiro_provider,
            compression_config,
            prompt_cache_runtime,
            prompt_runtime,
            truncation_recovery_notice,
            balance_cache: Mutex::new(balance_cache),
            cache_path,
            known_endpoints: known_endpoints.into_iter().collect(),
            balance_semaphore: Arc::new(Semaphore::new(8)),
            models_cache: Mutex::new(HashMap::new()),
            proxy_manager,
            import_jobs: Arc::new(Mutex::new(HashMap::new())),
            api_keys,
            config_path,
            metrics,
        }
    }

    /// 启动后并行预取所有未禁用凭据的余额，写入 disk-cache
    ///
    /// - 不复用运行时 `balance_semaphore`(cap 8)；启动期无其它流量，
    ///   用独立 `Semaphore(32)` 拉高启动并发，所有未禁用凭据近似同时发出
    /// - 已被磁盘缓存命中且未过期的凭据跳过，避免每次启动都打上游
    /// - 单条失败逐条降级，仅日志告警，不阻塞启动
    pub async fn prefetch_balances_on_startup(self: Arc<Self>) {
        let snapshot = self.token_manager.snapshot();
        let now_ts = Utc::now().timestamp() as f64;

        let stale_ids: Vec<u64> = {
            let cache = self.balance_cache.lock();
            snapshot
                .entries
                .iter()
                .filter(|e| !e.disabled)
                .filter_map(|e| {
                    let fresh = cache
                        .get(&e.id)
                        .map(|c| (now_ts - c.cached_at) < BALANCE_CACHE_TTL_SECS as f64)
                        .unwrap_or(false);
                    if fresh { None } else { Some(e.id) }
                })
                .collect()
        };

        if stale_ids.is_empty() {
            tracing::info!("启动余额预取：磁盘缓存命中所有凭据，跳过");
            return;
        }

        let total = stale_ids.len();
        tracing::info!("启动余额预取：{} 个凭据并行获取（cap=32）", total);
        let stats = self.refresh_balances_concurrent(stale_ids, 32).await;

        tracing::info!(
            "启动余额预取完成：成功 {}，失败 {}，低余额禁用 {}（共 {}）",
            stats.success,
            stats.failed,
            stats.low_balance_disabled,
            total
        );
    }

    /// 启动周期性余额刷新任务
    ///
    /// 每轮读 token_manager.config 的 `balance_refresh_*` 字段（支持热更新）：
    /// - `enabled=false`：跳过本轮拉取，仅 sleep 1 个 tick 后再读
    /// - `interval_secs`：触发周期（最小 180s，外部已 clamp）
    /// - `concurrency`：每批并发上限；凭据数 > 此值时 chunks 顺序分批，
    ///   上一批完成才进入下一批（避免单轮内同时占用过多上游连接）
    ///
    /// 写回 admin disk-cache + token_manager 运行时缓存。低余额自动禁用。
    /// 与启动预取共享 `refresh_balances_concurrent`，复用上游限流。
    pub fn start_periodic_balance_refresh(self: Arc<Self>) {
        tokio::spawn(async move {
            tracing::info!(
                "余额定时刷新已启动: 当前 enabled={}, 间隔={}s, 并发={}",
                self.token_manager.config().balance_refresh_enabled,
                self.token_manager.config().balance_refresh_interval_secs,
                self.token_manager.config().balance_refresh_concurrency,
            );
            // 启动后跳过首轮（启动预取已处理），先 sleep 一轮再开始
            let mut next_sleep =
                self.token_manager.config().balance_refresh_interval_secs.max(
                    crate::model::config::MIN_BALANCE_REFRESH_INTERVAL_SECS,
                );
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(next_sleep)).await;

                let cfg = self.token_manager.config();
                next_sleep = cfg
                    .balance_refresh_interval_secs
                    .max(crate::model::config::MIN_BALANCE_REFRESH_INTERVAL_SECS);

                if !cfg.balance_refresh_enabled {
                    tracing::debug!("余额定时刷新：已禁用，跳过本轮");
                    continue;
                }
                let concurrency = cfg
                    .balance_refresh_concurrency
                    .clamp(1, crate::model::config::MAX_BALANCE_REFRESH_CONCURRENCY);
                drop(cfg);

                let snapshot = self.token_manager.snapshot();
                let active_ids: Vec<u64> = snapshot
                    .entries
                    .iter()
                    .filter(|e| !e.disabled)
                    .map(|e| e.id)
                    .collect();
                if active_ids.is_empty() {
                    tracing::debug!("余额定时刷新：无活跃凭据");
                    continue;
                }
                let total = active_ids.len();
                // 凭据数 > concurrency 时分批，上一批完成才进入下一批
                let mut agg = RefreshBalanceStats::default();
                for chunk in active_ids.chunks(concurrency) {
                    let stats = self
                        .refresh_balances_concurrent(chunk.to_vec(), concurrency)
                        .await;
                    agg.success += stats.success;
                    agg.failed += stats.failed;
                    agg.low_balance_disabled += stats.low_balance_disabled;
                }
                tracing::info!(
                    "余额定时刷新完成：成功 {}，失败 {}，低余额禁用 {}（共 {}, 批大小 {}）",
                    agg.success,
                    agg.failed,
                    agg.low_balance_disabled,
                    total,
                    concurrency,
                );
            }
        });
    }

    /// 启动代理池健康巡检：每 interval_secs 经每个启用代理探测出口 IP，
    /// 连续失败 `PROXY_DEAD_THRESHOLD` 次判 dead 踢出调度；
    /// 刚判 dead 的代理上绑定的号，尝试按 region 自动换绑到其它可用代理。
    ///
    /// 探测有并发上限(默认 5)，避免一次性对所有代理发请求。
    pub fn start_proxy_health_patrol(self: Arc<Self>, interval_secs: u64) {
        const PATROL_CONCURRENCY: usize = 5;
        let interval = interval_secs.max(30);
        tokio::spawn(async move {
            tracing::info!("代理池健康巡检已启动: 间隔 {}s", interval);
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

                let ids = self.proxy_manager.enabled_ids();
                if ids.is_empty() {
                    continue;
                }

                let sem = Arc::new(Semaphore::new(PATROL_CONCURRENCY));
                let mut set = JoinSet::new();
                for id in ids {
                    let svc = Arc::clone(&self);
                    let sem = Arc::clone(&sem);
                    set.spawn(async move {
                        let _permit = sem.acquire().await.ok()?;
                        let entry = svc.proxy_manager.get(id)?;
                        let res = Self::probe_proxy(&entry).await;
                        // (just_dead, _recovered)
                        let (just_dead, _) =
                            svc.proxy_manager
                                .record_health(id, res.ok, res.error.clone());
                        Some((id, just_dead))
                    });
                }

                let mut newly_dead = Vec::new();
                while let Some(joined) = set.join_next().await {
                    if let Ok(Some((id, just_dead))) = joined {
                        if just_dead {
                            newly_dead.push(id);
                        }
                    }
                }

                // 刚判 dead 的代理 → 把它上面绑定的号按 region 换绑
                for dead_id in newly_dead {
                    let bound = self.token_manager.credentials_bound_to_proxy(dead_id);
                    if bound.is_empty() {
                        tracing::warn!("代理 #{} 判定 dead(无绑定号)", dead_id);
                        continue;
                    }
                    tracing::warn!(
                        "代理 #{} 判定 dead, 尝试为 {} 个绑定号换绑",
                        dead_id,
                        bound.len()
                    );
                    // 复用 auto_assign：强制重分这些号(reassign_bound=true)
                    let resp = self.auto_assign_proxies(ProxyAutoAssignRequest {
                        credential_ids: bound,
                        reassign_bound: true,
                    });
                    tracing::info!(
                        "死代理 #{} 换绑结果: 成功 {}, 无可用代理 {}",
                        dead_id,
                        resp.assigned.len(),
                        resp.skipped.len()
                    );
                }
            }
        });
    }

    ///
    /// - 用 `Semaphore(concurrency)` 限并发；每个成功项写回两层 cache 并同步运行时调度器
    /// - 余额低于 `LOW_BALANCE_THRESHOLD` 自动禁用
    /// - 完成后一次性 `save_balance_cache`
    async fn refresh_balances_concurrent(
        self: &Arc<Self>,
        ids: Vec<u64>,
        concurrency: usize,
    ) -> RefreshBalanceStats {
        let sem = Arc::new(Semaphore::new(concurrency.max(1)));
        let mut tasks: JoinSet<(u64, Option<BalanceResponse>, Option<String>)> = JoinSet::new();

        for id in ids {
            let token_manager = self.token_manager.clone();
            let sem = sem.clone();
            tasks.spawn(async move {
                let _permit = match sem.acquire().await {
                    Ok(p) => p,
                    Err(e) => return (id, None, Some(format!("acquire semaphore: {e}"))),
                };
                match token_manager.get_usage_limits_for(id).await {
                    Ok(usage) => {
                        let current_usage = usage.current_usage();
                        let usage_limit = usage.usage_limit();
                        let remaining = (usage_limit - current_usage).max(0.0);
                        let usage_percentage = if usage_limit > 0.0 {
                            (current_usage / usage_limit * 100.0).min(100.0)
                        } else {
                            0.0
                        };
                        let resp = BalanceResponse {
                            id,
                            subscription_title: usage.subscription_title().map(|s| s.to_string()),
                            current_usage,
                            usage_limit,
                            remaining,
                            usage_percentage,
                            next_reset_at: usage.next_date_reset,
                            overage_cap: usage.overage_cap(),
                            overage_capability: usage.overage_capability().map(|s| s.to_string()),
                            overage_status: usage.overage_status().map(|s| s.to_string()),
                        };
                        (id, Some(resp), None)
                    }
                    Err(e) => (id, None, Some(e.to_string())),
                }
            });
        }

        let mut stats = RefreshBalanceStats::default();
        let cache_now = Utc::now().timestamp() as f64;
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((id, Some(balance), _)) => {
                    let remaining = balance.remaining;
                    let overage_rem = overage_remaining(&balance);
                    {
                        let mut cache = self.balance_cache.lock();
                        cache.insert(
                            id,
                            CachedBalance {
                                cached_at: cache_now,
                                data: balance,
                            },
                        );
                    }
                    self.token_manager
                        .update_balance_cache_full(id, remaining, overage_rem);
                    // 真正不可用 = 正式额度耗尽 AND（超额未开启 OR 超额额度耗尽）
                    let exhausted = remaining < LOW_BALANCE_THRESHOLD
                        && overage_rem < LOW_BALANCE_THRESHOLD;
                    if exhausted {
                        self.token_manager.mark_insufficient_balance(id);
                        stats.low_balance_disabled += 1;
                        tracing::warn!(
                            "凭据 #{} 额度耗尽（正式 {:.2}, 超额 remaining={:.2}），已自动禁用",
                            id,
                            remaining,
                            overage_rem
                        );
                    } else {
                        tracing::debug!(
                            "凭据 #{} 余额已刷新: 正式 {:.2}, 超额 remaining={:.2}",
                            id,
                            remaining,
                            overage_rem
                        );
                    }
                    stats.success += 1;
                }
                Ok((id, None, err)) => {
                    stats.failed += 1;
                    tracing::warn!(
                        "余额刷新失败 #{}: {}",
                        id,
                        err.unwrap_or_else(|| "unknown".to_string())
                    );
                }
                Err(e) => {
                    stats.failed += 1;
                    tracing::warn!("余额刷新 task join 失败: {}", e);
                }
            }
        }
        self.save_balance_cache();
        stats
    }


    /// 获取所有凭据状态
    pub fn get_all_credentials(&self) -> CredentialsStatusResponse {
        let snapshot = self.token_manager.snapshot();
        let default_endpoint = self.token_manager.config().default_endpoint.clone();

        let mut credentials: Vec<CredentialStatusItem> = snapshot
            .entries
            .into_iter()
            .map(|entry| CredentialStatusItem {
                id: entry.id,
                priority: entry.priority,
                disabled: entry.disabled,
                failure_count: entry.failure_count,
                expires_at: entry.expires_at,
                auth_method: entry.auth_method,
                has_profile_arn: entry.has_profile_arn,
                refresh_token_hash: entry.refresh_token_hash,
                api_key_hash: entry.api_key_hash,
                masked_api_key: entry.masked_api_key,
                email: entry.email,
                success_count: entry.success_count,
                last_used_at: entry.last_used_at.clone(),
                has_proxy: entry.has_proxy,
                proxy_url: entry.proxy_url,
                proxy_id: entry.proxy_id,
                refresh_failure_count: entry.refresh_failure_count,
                disabled_reason: entry.disabled_reason,
                endpoint: entry.endpoint.unwrap_or_else(|| default_endpoint.clone()),
                available_permits: entry.available_permits,
                max_permits: entry.max_permits,
                concurrency: entry.concurrency,
                recovery_attempts: entry.recovery_attempts,
                next_retry_at: entry.next_retry_at,
                group: entry.group,
                source: entry.source,
            })
            .collect();

        // 按优先级排序（数字越小优先级越高）
        credentials.sort_by_key(|c| c.priority);

        CredentialsStatusResponse {
            total: snapshot.total,
            available: snapshot.available,
            credentials,
        }
    }

    /// 设置凭据禁用状态
    pub fn set_disabled(&self, id: u64, disabled: bool) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_disabled(id, disabled)
            .map_err(|e| self.classify_error(e, id))?;
        Ok(())
    }

    /// 设置凭据优先级
    pub fn set_priority(&self, id: u64, priority: u32) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_priority(id, priority)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置单凭据独立并发上限（None=回退到全局 per_credential_concurrency）
    pub fn set_concurrency(
        &self,
        id: u64,
        concurrency: Option<u32>,
    ) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_credential_concurrency(id, concurrency)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 重置失败计数并重新启用
    pub fn reset_and_enable(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .reset_and_enable(id)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 获取凭据余额（带缓存）
    pub async fn get_balance(
        &self,
        id: u64,
        force: bool,
    ) -> Result<BalanceResponse, AdminServiceError> {
        // force=true 跳过缓存，直接走云端
        if !force {
            let cache = self.balance_cache.lock();
            if let Some(cached) = cache.get(&id) {
                let now = Utc::now().timestamp() as f64;
                if (now - cached.cached_at) < BALANCE_CACHE_TTL_SECS as f64 {
                    tracing::debug!("凭据 #{} 余额命中缓存", id);
                    return Ok(cached.data.clone());
                }
            }
        }

        // 缓存未命中或已过期，从上游获取
        let balance = self.fetch_balance(id).await?;

        // 更新 admin 端展示缓存
        {
            let mut cache = self.balance_cache.lock();
            cache.insert(
                id,
                CachedBalance {
                    cached_at: Utc::now().timestamp() as f64,
                    data: balance.clone(),
                },
            );
        }
        self.save_balance_cache();

        // 同步调度器运行时余额缓存（rank_candidates 派送依据）
        self.token_manager.update_balance_cache_full(
            id,
            balance.remaining,
            overage_remaining(&balance),
        );

        Ok(balance)
    }

    /// 从上游获取余额（无缓存）
    ///
    /// 异步队列设计：
    /// 1. 先查 snapshot：disabled 凭据直接返回 `InvalidCredential`，不进队列
    /// 2. 通过 `balance_semaphore` 限流（与批量刷新共享，全局并发上限 8）
    /// 3. 拿到 permit 后调用 `get_usage_limits_for`；permit 在函数返回时自动释放
    async fn fetch_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        // disabled 凭据快速失败：不占用队列槽位
        let snapshot = self.token_manager.snapshot();
        if let Some(entry) = snapshot.entries.iter().find(|e| e.id == id) {
            if entry.disabled {
                return Err(AdminServiceError::InvalidCredential(format!(
                    "credential {id} disabled"
                )));
            }
        } else {
            return Err(AdminServiceError::NotFound { id });
        }

        // 进入余额查询队列：拿到 permit 才能继续，超出并发的请求在此排队
        let _permit = self
            .balance_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| {
                AdminServiceError::InternalError(format!("acquire balance semaphore failed: {e}"))
            })?;

        let usage = self
            .token_manager
            .get_usage_limits_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;

        let current_usage = usage.current_usage();
        let usage_limit = usage.usage_limit();
        let remaining = (usage_limit - current_usage).max(0.0);
        let usage_percentage = if usage_limit > 0.0 {
            (current_usage / usage_limit * 100.0).min(100.0)
        } else {
            0.0
        };

        Ok(BalanceResponse {
            id,
            subscription_title: usage.subscription_title().map(|s| s.to_string()),
            current_usage,
            usage_limit,
            remaining,
            usage_percentage,
            next_reset_at: usage.next_date_reset,
            overage_cap: usage.overage_cap(),
            overage_capability: usage.overage_capability().map(|s| s.to_string()),
            overage_status: usage.overage_status().map(|s| s.to_string()),
        })
    }

    /// 拉取指定凭据可用模型列表（30 分钟内存缓存；force=true 跳过缓存）
    ///
    /// API Key 凭据 / 不存在 / 被禁用 → InvalidCredential / NotFound；
    /// 上游 401 / 403 → UpstreamError，由前端展示。
    pub async fn list_available_models(
        &self,
        id: u64,
        model_provider: Option<&str>,
        force: bool,
    ) -> Result<crate::kiro::models::ListAvailableModelsResponse, AdminServiceError> {
        {
            let snapshot = self.token_manager.snapshot();
            let entry = snapshot
                .entries
                .iter()
                .find(|e| e.id == id)
                .ok_or(AdminServiceError::NotFound { id })?;
            if entry.disabled {
                return Err(AdminServiceError::InvalidCredential(format!(
                    "credential {id} disabled"
                )));
            }
        }

        let key = (id, model_provider.map(str::to_string));
        if !force {
            if let Some(cached) = self.models_cache.lock().get(&key) {
                if cached.cached_at.elapsed().as_secs() < MODELS_CACHE_TTL_SECS {
                    return Ok(cached.data.clone());
                }
            }
        }

        let response = self
            .token_manager
            .list_available_models_for(id, model_provider)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;

        self.models_cache.lock().insert(
            key,
            CachedModels {
                cached_at: std::time::Instant::now(),
                data: response.clone(),
            },
        );
        Ok(response)
    }

    /// 获取所有凭据的缓存余额
    ///
    /// 双源合并：
    /// - `token_manager` 提供运行时缓存（cached_at + 动态 ttl_secs）
    /// - `AdminService` 自身的 disk-backed 5 分钟缓存提供完整快照（usage_limit /
    ///   usage_percentage / subscription_title），保证字段一致性
    pub fn get_cached_balances(&self) -> CachedBalancesResponse {
        // 从 token_manager 获取运行时缓存（含 TTL 信息）
        let runtime_balances: HashMap<u64, CachedBalanceInfo> = self
            .token_manager
            .get_all_cached_balances()
            .into_iter()
            .map(|info| (info.id, info))
            .collect();

        // 以 entries 为基准遍历，磁盘缓存提供完整数据，运行时缓存提供 cached_at/ttl
        // 任一来源命中即返回该凭据的余额条目
        let snapshot_ids: Vec<u64> = self
            .token_manager
            .snapshot()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();

        let disk_cache = self.balance_cache.lock();
        let now_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let balances = snapshot_ids
            .into_iter()
            .filter_map(|id| {
                let runtime = runtime_balances.get(&id);
                let disk = disk_cache.get(&id);
                if runtime.is_none() && disk.is_none() {
                    return None;
                }

                // cached_at 优先用 runtime，其次用 disk 自带时间戳
                let (cached_at, ttl_secs) = match runtime {
                    Some(r) => (r.cached_at, r.ttl_secs),
                    None => {
                        let disk_at_ms = disk
                            .map(|d| (d.cached_at * 1000.0) as u64)
                            .unwrap_or(now_unix_ms);
                        // 启动后磁盘命中但 runtime 缺失：用 BALANCE_CACHE_TTL_SECS 兜底
                        (disk_at_ms, BALANCE_CACHE_TTL_SECS as u64)
                    }
                };

                let item = if let Some(cached) = disk {
                    CachedBalanceItem {
                        id,
                        current_usage: cached.data.current_usage,
                        usage_limit: cached.data.usage_limit,
                        remaining: cached.data.remaining,
                        usage_percentage: cached.data.usage_percentage,
                        subscription_title: cached.data.subscription_title.clone(),
                        next_reset_at: cached.data.next_reset_at,
                        overage_cap: cached.data.overage_cap,
                        overage_capability: cached.data.overage_capability.clone(),
                        overage_status: cached.data.overage_status.clone(),
                        cached_at,
                        ttl_secs,
                    }
                } else {
                    let r = runtime.unwrap();
                    CachedBalanceItem {
                        id,
                        current_usage: 0.0,
                        usage_limit: 0.0,
                        remaining: r.remaining,
                        usage_percentage: 0.0,
                        subscription_title: None,
                        next_reset_at: None,
                        overage_cap: 0.0,
                        overage_capability: None,
                        overage_status: None,
                        cached_at,
                        ttl_secs,
                    }
                };
                Some(item)
            })
            .collect();

        CachedBalancesResponse { balances }
    }

    /// 添加新凭据
    pub async fn add_credential(
        &self,
        req: AddCredentialRequest,
    ) -> Result<AddCredentialResponse, AdminServiceError> {
        // 校验端点名：未指定则默认合法，指定则必须已注册
        if let Some(ref name) = req.endpoint {
            if !self.known_endpoints.contains(name) {
                let mut known: Vec<&str> =
                    self.known_endpoints.iter().map(|s| s.as_str()).collect();
                known.sort();
                return Err(AdminServiceError::InvalidCredential(format!(
                    "未知端点 \"{}\"，已注册端点: {:?}",
                    name, known
                )));
            }
        }

        // 构建凭据对象
        let email = req.email.clone();
        let new_cred = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: req.refresh_token,
            profile_arn: None,
            expires_at: None,
            auth_method: Some(req.auth_method),
            client_id: req.client_id,
            client_secret: req.client_secret,
            priority: req.priority,
            region: req.region,
            auth_region: req.auth_region,
            api_region: req.api_region,
            machine_id: req.machine_id,
            email: req.email,
            subscription_title: None, // 将在首次获取使用额度时自动更新
            proxy_url: req.proxy_url,
            proxy_username: req.proxy_username,
            proxy_password: req.proxy_password,
            proxy_id: req.proxy_id,
            disabled: false, // 新添加的凭据默认启用
            kiro_api_key: req.kiro_api_key,
            endpoint: req.endpoint,
            concurrency: req.concurrency,
            group: req.group.clone(),
            source: req.source.clone(),
        };

        // 调用 token_manager 添加凭据
        let credential_id = self
            .token_manager
            .add_credential(new_cred, true)
            .await
            .map_err(|e| self.classify_add_error(e))?;

        // 主动获取订阅等级，避免首次请求时 Free 账号绕过 Opus 模型过滤
        if let Err(e) = self.token_manager.get_usage_limits_for(credential_id).await {
            tracing::warn!("添加凭据后获取订阅等级失败（不影响凭据添加）: {}", e);
        }

        // 导入即默认开启超额(overage)。失败不影响凭据添加(用户可后续手动开)。
        if let Err(e) = self
            .token_manager
            .set_overage_status_for(credential_id, true)
            .await
        {
            tracing::warn!("凭据 #{} 添加后默认开启超额失败(不影响添加): {}", credential_id, e);
        }

        Ok(AddCredentialResponse {
            success: true,
            message: format!("凭据添加成功，ID: {}", credential_id),
            credential_id,
            email,
        })
    }

    /// 删除凭据
    pub fn delete_credential(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .delete_credential(id)
            .map_err(|e| self.classify_delete_error(e, id))?;

        // 清理已删除凭据的余额缓存
        {
            let mut cache = self.balance_cache.lock();
            cache.remove(&id);
        }
        self.save_balance_cache();

        Ok(())
    }

    /// 批量删除凭据(任意状态均可删)。逐个删除,单条失败不影响其余,汇总结果。
    pub fn delete_credentials_batch(
        &self,
        ids: Vec<u64>,
    ) -> crate::admin::types::BatchDeleteResponse {
        use crate::admin::types::{BatchDeleteResponse, BatchDeleteResultItem};
        let mut results = Vec::with_capacity(ids.len());
        let mut success_count = 0usize;
        let mut failure_count = 0usize;
        for id in ids {
            match self.delete_credential(id) {
                Ok(_) => {
                    success_count += 1;
                    results.push(BatchDeleteResultItem {
                        id,
                        success: true,
                        error: None,
                    });
                }
                Err(e) => {
                    failure_count += 1;
                    results.push(BatchDeleteResultItem {
                        id,
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        BatchDeleteResponse {
            results,
            success_count,
            failure_count,
        }
    }

    /// 强制刷新指定凭据的 Token
    pub async fn force_refresh_token(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .force_refresh_token_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))
    }

    /// 切换上游 overage 开关（调用 Kiro `setUserPreference`）
    ///
    /// 上游会自行校验资格（INCAPABLE 订阅会返回 4xx）。这里直接透传上游错误，
    /// 不在 admin 侧做资格预检——避免和余额缓存 TTL/未刷新的状态产生不一致。
    pub async fn set_overage_status(
        &self,
        id: u64,
        enabled: bool,
    ) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_overage_status_for(id, enabled)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;

        // 1) 清磁盘缓存：避免后续 get_balance 走 TTL 命中老值
        let removed = {
            let mut cache = self.balance_cache.lock();
            cache.remove(&id).is_some()
        };
        if removed {
            self.save_balance_cache();
        }
        // 2) 清 token_manager 运行时缓存（标记 initialized=false）
        self.token_manager.invalidate_balance_cache(id);

        // 3) 主动拉新值回填两个 cache：overage 开关会改 cap / overage_status，
        //    调度器和前端都需要尽快看到最新值，不能等下一次 get_balance 触发
        match self.fetch_balance(id).await {
            Ok(balance) => {
                {
                    let mut cache = self.balance_cache.lock();
                    cache.insert(
                        id,
                        CachedBalance {
                            cached_at: Utc::now().timestamp() as f64,
                            data: balance.clone(),
                        },
                    );
                }
                self.save_balance_cache();
                self.token_manager.update_balance_cache_full(
                    id,
                    balance.remaining,
                    overage_remaining(&balance),
                );
            }
            Err(e) => {
                // 拉新失败不阻塞 overage 切换成功的语义；下一轮 should_refresh_balance 会兜底
                tracing::warn!("overage 切换后拉新余额失败 #{}: {}", id, e);
            }
        }

        Ok(())
    }

    /// 轻量运行时状态快照（高频轮询用，纯内存读取，不触发任何 IO）
    ///
    /// 字段精简至 dashboard 实时需要的 5 项：
    /// - `id`：凭据主键
    /// - `last_used_at`：最近一次被选中的时间戳（RFC3339）
    /// - `available_permits` / `max_permits`：用于渲染 K/N 并发占用
    /// - `disabled`：手动禁用标记
    pub fn get_runtime_stats(&self) -> RuntimeStatsResponse {
        let snapshot = self.token_manager.snapshot();
        let disk_cache = self.balance_cache.lock();
        let credentials = snapshot
            .entries
            .into_iter()
            .map(|entry| {
                let balance = disk_cache.get(&entry.id).map(|cached| RuntimeBalanceSnapshot {
                    subscription_title: cached.data.subscription_title.clone(),
                    current_usage: cached.data.current_usage,
                    usage_limit: cached.data.usage_limit,
                    remaining: cached.data.remaining,
                    usage_percentage: cached.data.usage_percentage,
                    next_reset_at: cached.data.next_reset_at,
                    overage_cap: cached.data.overage_cap,
                    overage_capability: cached.data.overage_capability.clone(),
                    overage_status: cached.data.overage_status.clone(),
                });
                RuntimeStatsItem {
                    id: entry.id,
                    last_used_at: entry.last_used_at.clone(),
                    available_permits: entry.available_permits,
                    max_permits: entry.max_permits,
                    disabled: entry.disabled,
                    balance,
                }
            })
            .collect();
        RuntimeStatsResponse { credentials }
    }

    /// 批量强制刷新 Token（B 端点）
    ///
    /// 用 `Semaphore(8)` 限制并发，`JoinSet` 收集结果。
    /// 单个失败不影响其他凭据，全部完成后返回 `BatchRefreshResponse`。
    /// 内部调用 `force_refresh_token_for(id)`，对 API Key 凭据会 `bail` 走 Err 分支。
    pub async fn force_refresh_tokens_batch(&self, ids: Vec<u64>) -> BatchRefreshResponse {
        // 源头过滤：禁用的凭据直接跳过刷新，不占用并发槽位
        let snapshot = self.token_manager.snapshot();
        let disabled_ids: HashSet<u64> = snapshot
            .entries
            .iter()
            .filter(|e| e.disabled)
            .map(|e| e.id)
            .collect();
        let (active_ids, skipped_ids): (Vec<u64>, Vec<u64>) = ids
            .into_iter()
            .partition(|id| !disabled_ids.contains(id));

        let semaphore = Arc::new(Semaphore::new(8));
        let mut tasks: JoinSet<BatchRefreshResultItem> = JoinSet::new();

        for id in active_ids {
            let token_manager = self.token_manager.clone();
            let semaphore = semaphore.clone();
            tasks.spawn(async move {
                // 获取并发许可（最多 8 个并发刷新）
                let _permit = match semaphore.acquire().await {
                    Ok(p) => p,
                    Err(e) => {
                        return BatchRefreshResultItem {
                            id,
                            success: false,
                            error: Some(format!("acquire semaphore failed: {e}")),
                        };
                    }
                };
                match token_manager.force_refresh_token_for(id).await {
                    Ok(()) => BatchRefreshResultItem {
                        id,
                        success: true,
                        error: None,
                    },
                    Err(e) => BatchRefreshResultItem {
                        id,
                        success: false,
                        error: Some(e.to_string()),
                    },
                }
            });
        }

        let mut results = Vec::new();
        let mut success_count = 0usize;
        let mut failure_count = skipped_ids.len();
        // 禁用条目直接构造失败项加入结果集
        for id in skipped_ids {
            results.push(BatchRefreshResultItem {
                id,
                success: false,
                error: Some("credential disabled".to_string()),
            });
        }
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(item) => {
                    if item.success {
                        success_count += 1;
                    } else {
                        failure_count += 1;
                    }
                    results.push(item);
                }
                Err(e) => {
                    failure_count += 1;
                    results.push(BatchRefreshResultItem {
                        id: 0,
                        success: false,
                        error: Some(format!("task join error: {e}")),
                    });
                }
            }
        }
        // 按 id 升序便于前端展示
        results.sort_by_key(|r| r.id);

        BatchRefreshResponse {
            results,
            success_count,
            failure_count,
        }
    }

    /// 批量强制刷新余额（不入缓存）
    ///
    /// 用 `Semaphore(8)` 限制并发，`JoinSet` 收集结果。
    /// 单个失败不影响其他凭据，全部完成后返回 `BatchRefreshBalanceResponse`。
    /// 内部直接调用 `token_manager.get_usage_limits_for(id)` 获取最新值，
    /// 不写入余额缓存（与单条 force-refresh 余额端点不同，避免大批量回写抖动）。
    pub async fn force_refresh_balances_batch(
        &self,
        ids: Vec<u64>,
    ) -> BatchRefreshBalanceResponse {
        // 源头过滤：禁用的凭据直接跳过查询，不占用并发槽位
        let snapshot = self.token_manager.snapshot();
        let disabled_ids: HashSet<u64> = snapshot
            .entries
            .iter()
            .filter(|e| e.disabled)
            .map(|e| e.id)
            .collect();
        let (active_ids, skipped_ids): (Vec<u64>, Vec<u64>) = ids
            .into_iter()
            .partition(|id| !disabled_ids.contains(id));

        // 与单条 fetch_balance 共享同一个全局余额查询 Semaphore（容量 8），
        // 避免批量刷新与零散查询互相抢占
        let semaphore = self.balance_semaphore.clone();
        let mut tasks: JoinSet<BatchRefreshBalanceResultItem> = JoinSet::new();

        for id in active_ids {
            let token_manager = self.token_manager.clone();
            let semaphore = semaphore.clone();
            tasks.spawn(async move {
                // 获取并发许可（最多 8 个并发查询）
                let _permit = match semaphore.acquire().await {
                    Ok(p) => p,
                    Err(e) => {
                        return BatchRefreshBalanceResultItem {
                            id,
                            success: false,
                            balance: None,
                            error: Some(format!("acquire semaphore failed: {e}")),
                        };
                    }
                };
                match token_manager.get_usage_limits_for(id).await {
                    Ok(usage) => {
                        // 字段聚合复用 UsageLimitsResponse 的便捷方法，
                        // 公式与 AdminService::fetch_balance 保持一致
                        let current_usage = usage.current_usage();
                        let usage_limit = usage.usage_limit();
                        let remaining = (usage_limit - current_usage).max(0.0);
                        let usage_percentage = if usage_limit > 0.0 {
                            (current_usage / usage_limit * 100.0).min(100.0)
                        } else {
                            0.0
                        };
                        BatchRefreshBalanceResultItem {
                            id,
                            success: true,
                            balance: Some(BalanceResponse {
                                id,
                                subscription_title: usage
                                    .subscription_title()
                                    .map(|s| s.to_string()),
                                current_usage,
                                usage_limit,
                                remaining,
                                usage_percentage,
                                next_reset_at: usage.next_date_reset,
                                overage_cap: usage.overage_cap(),
                                overage_capability: usage
                                    .overage_capability()
                                    .map(|s| s.to_string()),
                                overage_status: usage.overage_status().map(|s| s.to_string()),
                            }),
                            error: None,
                        }
                    }
                    Err(e) => BatchRefreshBalanceResultItem {
                        id,
                        success: false,
                        balance: None,
                        error: Some(e.to_string()),
                    },
                }
            });
        }

        let mut results = Vec::new();
        let mut success_count = 0usize;
        let mut failure_count = skipped_ids.len();
        // 禁用条目直接构造失败项加入结果集
        for id in skipped_ids {
            results.push(BatchRefreshBalanceResultItem {
                id,
                success: false,
                balance: None,
                error: Some("credential disabled".to_string()),
            });
        }
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(item) => {
                    if item.success {
                        success_count += 1;
                    } else {
                        failure_count += 1;
                    }
                    results.push(item);
                }
                Err(e) => {
                    failure_count += 1;
                    results.push(BatchRefreshBalanceResultItem {
                        id: 0,
                        success: false,
                        balance: None,
                        error: Some(format!("task join error: {e}")),
                    });
                }
            }
        }
        // 按 id 升序便于前端展示
        results.sort_by_key(|r| r.id);

        // 批量成功项写回磁盘缓存（与单条 force-refresh 余额端点一致），
        // 让启动后预取与 GET /balances/cached 始终能读到最新快照
        let now_ts = Utc::now().timestamp() as f64;
        {
            let mut cache = self.balance_cache.lock();
            for item in &results {
                if let (true, Some(balance)) = (item.success, item.balance.as_ref()) {
                    cache.insert(
                        item.id,
                        CachedBalance {
                            cached_at: now_ts,
                            data: balance.clone(),
                        },
                    );
                }
            }
        }
        self.save_balance_cache();

        // 同步调度器运行时余额缓存（rank_candidates 派送依据）
        for item in &results {
            if let (true, Some(balance)) = (item.success, item.balance.as_ref()) {
                self.token_manager
                    .update_balance_cache_full(item.id, balance.remaining, overage_remaining(balance));
            }
        }

        BatchRefreshBalanceResponse {
            results,
            success_count,
            failure_count,
        }
    }

    // ============ 余额缓存持久化 ============

    fn load_balance_cache_from(cache_path: &Option<PathBuf>) -> HashMap<u64, CachedBalance> {
        let path = match cache_path {
            Some(p) => p,
            None => return HashMap::new(),
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };

        // 文件中使用字符串 key 以兼容 JSON 格式
        let map: HashMap<String, CachedBalance> = match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("解析余额缓存失败，将忽略: {}", e);
                return HashMap::new();
            }
        };

        let now = Utc::now().timestamp() as f64;
        map.into_iter()
            .filter_map(|(k, v)| {
                let id = k.parse::<u64>().ok()?;
                // 丢弃超过 TTL 的条目
                if (now - v.cached_at) < BALANCE_CACHE_TTL_SECS as f64 {
                    Some((id, v))
                } else {
                    None
                }
            })
            .collect()
    }

    fn save_balance_cache(&self) {
        let path = match &self.cache_path {
            Some(p) => p,
            None => return,
        };

        // 持有锁期间完成序列化和写入，防止并发损坏
        let cache = self.balance_cache.lock();
        let map: HashMap<String, &CachedBalance> =
            cache.iter().map(|(k, v)| (k.to_string(), v)).collect();

        match serde_json::to_string_pretty(&map) {
            Ok(json) => {
                if let Err(e) = crate::common::io::atomic_write_string(path, &json) {
                    tracing::warn!("保存余额缓存失败: {}", e);
                }
            }
            Err(e) => tracing::warn!("序列化余额缓存失败: {}", e),
        }
    }

    // ============ 错误分类 ============

    /// 分类简单操作错误（set_disabled, set_priority, reset_and_enable）
    fn classify_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类余额查询错误（可能涉及上游 API 调用）
    fn classify_balance_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();

        // 1. 凭据不存在
        if msg.contains("不存在") {
            return AdminServiceError::NotFound { id };
        }

        // 2. API Key 凭据不支持刷新：客户端请求错误，映射为 400
        if msg.contains("API Key 凭据不支持刷新") {
            return AdminServiceError::InvalidCredential(msg);
        }

        // 3. 上游服务错误特征：HTTP 响应错误或网络错误
        let is_upstream_error =
            // HTTP 响应错误（来自 refresh_*_token 的错误消息）
            msg.contains("凭证已过期或无效") ||
            msg.contains("权限不足") ||
            msg.contains("已被限流") ||
            msg.contains("服务器错误") ||
            msg.contains("Token 刷新失败") ||
            msg.contains("暂时不可用") ||
            // 网络错误（reqwest 错误）
            msg.contains("error trying to connect") ||
            msg.contains("connection") ||
            msg.contains("timeout") ||
            msg.contains("timed out");

        if is_upstream_error {
            AdminServiceError::UpstreamError(msg)
        } else {
            // 4. 默认归类为内部错误（本地验证失败、配置错误等）
            // 包括：缺少 refreshToken、refreshToken 已被截断、无法生成 machineId 等
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类添加凭据错误
    fn classify_add_error(&self, e: anyhow::Error) -> AdminServiceError {
        let msg = e.to_string();

        // 凭据验证失败（refreshToken 无效、格式错误等）
        let is_invalid_credential = msg.contains("缺少 refreshToken")
            || msg.contains("refreshToken 为空")
            || msg.contains("refreshToken 已被截断")
            || msg.contains("凭据已存在")
            || msg.contains("refreshToken 重复")
            || msg.contains("kiroApiKey 重复")
            || msg.contains("缺少 kiroApiKey")
            || msg.contains("kiroApiKey 为空")
            || msg.contains("凭证已过期或无效")
            || msg.contains("权限不足")
            || msg.contains("已被限流");

        if is_invalid_credential {
            AdminServiceError::InvalidCredential(msg)
        } else if msg.contains("error trying to connect")
            || msg.contains("connection")
            || msg.contains("timeout")
        {
            AdminServiceError::UpstreamError(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类删除凭据错误
    fn classify_delete_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else if msg.contains("只能删除已禁用的凭据") || msg.contains("请先禁用凭据") {
            AdminServiceError::InvalidCredential(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    // ============ 全局代理配置（热更新） ============

    /// 设置凭据 Region（凭据级 region/api_region 覆盖）
    pub fn set_region(
        &self,
        id: u64,
        region: Option<String>,
        api_region: Option<String>,
    ) -> Result<(), AdminServiceError> {
        let region = region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let api_region = api_region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.token_manager
            .set_region(id, region, api_region)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据 endpoint（凭据级 endpoint 覆盖，须命中已注册端点）
    pub fn set_endpoint(
        &self,
        id: u64,
        endpoint: Option<String>,
    ) -> Result<(), AdminServiceError> {
        let endpoint = endpoint
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(name) = endpoint.as_deref()
            && !self.known_endpoints.contains(name)
        {
            let mut known: Vec<&str> = self.known_endpoints.iter().map(|s| s.as_str()).collect();
            known.sort_unstable();
            return Err(AdminServiceError::InvalidCredential(format!(
                "endpoint 必须是已注册值，已注册: {:?}，收到: {}",
                known, name
            )));
        }

        self.token_manager
            .set_endpoint(id, endpoint)
            .map_err(|e| self.classify_error(e, id))
    }

    pub fn set_group(&self, id: u64, group: Option<String>) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_group(id, group)
            .map_err(|e| self.classify_error(e, id))
    }

    pub fn set_source(&self, id: u64, source: Option<String>) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_source(id, source)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 批量禁用/启用凭据
    pub fn disable_credentials_batch(
        &self,
        ids: &[u64],
        disabled: bool,
    ) -> DisableBatchResponse {
        let mut success_count = 0usize;
        let mut failure_count = 0usize;
        for &id in ids {
            match self.set_disabled(id, disabled) {
                Ok(_) => success_count += 1,
                Err(_) => failure_count += 1,
            }
        }
        DisableBatchResponse {
            success_count,
            failure_count,
        }
    }

    /// 获取当前代理配置（脱敏）
    pub fn get_proxy_config(&self) -> ProxyConfigResponse {
        let config = self.token_manager.config();
        ProxyConfigResponse {
            proxy_url: config.proxy_url.clone(),
            has_credentials: config.proxy_username.is_some()
                && config.proxy_password.is_some(),
        }
    }

    /// 更新代理配置（热更新）
    pub async fn update_proxy_config(
        &self,
        req: UpdateProxyConfigRequest,
    ) -> Result<(), AdminServiceError> {
        // 1. 构建新的 ProxyConfig
        let new_proxy = if let Some(url) = &req.proxy_url {
            if url.trim().is_empty() {
                None
            } else {
                let mut proxy = ProxyConfig::new(url.trim());
                if let (Some(u), Some(p)) = (&req.proxy_username, &req.proxy_password)
                    && !u.trim().is_empty()
                    && !p.trim().is_empty()
                {
                    proxy = proxy.with_auth(u.trim(), p.trim());
                }
                // 如果未提供新认证信息，保留现有认证
                if proxy.username.is_none() {
                    let config = self.token_manager.config();
                    if let (Some(u), Some(p)) =
                        (&config.proxy_username, &config.proxy_password)
                    {
                        proxy = proxy.with_auth(u, p);
                    }
                }
                Some(proxy)
            }
        } else {
            None
        };

        // 2. 先持久化配置（失败时不影响运行时状态）
        self.token_manager.with_config_mut(|cfg| {
            cfg.proxy_url = new_proxy.as_ref().map(|p| p.url.clone());
            cfg.proxy_username = new_proxy.as_ref().and_then(|p| p.username.clone());
            cfg.proxy_password = new_proxy.as_ref().and_then(|p| p.password.clone());
            cfg.save()
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))
        })?;

        // 3. 持久化成功后再应用运行时变更
        // 贴合 BK admin/service.rs:785-808：先 token_manager 后 provider 双层同步
        self.token_manager.update_proxy(new_proxy.clone());
        if let Some(provider) = &self.kiro_provider {
            if let Err(e) = provider.update_global_proxy(new_proxy) {
                tracing::warn!("provider.update_global_proxy 失败（已持久化）: {}", e);
            }
        }

        Ok(())
    }

    /// 获取全局配置
    pub fn get_global_config(&self) -> GlobalConfigResponse {
        let config = self.token_manager.config();
        let c = self.compression_config.read();
        GlobalConfigResponse {
            region: config.region.clone(),
            prompt_cache_ttl_seconds: config.prompt_cache_ttl_seconds,
            prompt_cache_accounting_enabled: config.prompt_cache_accounting_enabled,
            default_endpoint: config.default_endpoint.clone(),
            extract_thinking: config.extract_thinking,
            per_credential_concurrency: config.per_credential_concurrency,
            global_concurrency: config.global_concurrency,
            acquire_wait_timeout_secs: config.acquire_wait_timeout_secs,
            balance_refresh_enabled: config.balance_refresh_enabled,
            balance_refresh_interval_secs: config.balance_refresh_interval_secs,
            balance_refresh_concurrency: config.balance_refresh_concurrency,
            session_affinity_enabled: config.session_affinity_enabled,
            truncation_recovery_system_notice: config.truncation_recovery_system_notice,
            privacy_mode: config.privacy_mode,
            compression: CompressionConfigResponse {
                enabled: c.enabled,
                whitespace_compression: c.whitespace_compression,
                thinking_strategy: c.thinking_strategy.clone(),
                tool_result_max_chars: c.tool_result_max_chars,
                tool_result_head_lines: c.tool_result_head_lines,
                tool_result_tail_lines: c.tool_result_tail_lines,
                tool_use_input_max_chars: c.tool_use_input_max_chars,
                tool_description_max_chars: c.tool_description_max_chars,
                max_history_turns: c.max_history_turns,
                max_history_chars: c.max_history_chars,
                image_max_long_edge: c.image_max_long_edge,
                image_max_pixels_single: c.image_max_pixels_single,
                image_max_pixels_multi: c.image_max_pixels_multi,
                image_multi_threshold: c.image_multi_threshold,
                image_compression_enabled: c.image_compression_enabled,
                max_request_body_bytes: c.max_request_body_bytes,
            },
        }
    }

    /// 更新全局配置（热更新）
    ///
    /// 返回更新后的 `GlobalConfigResponse`，前端拿到即可直接渲染，避免
    /// 再发一次 GET 请求；与 `get_global_config()` 同形。
    pub async fn update_global_config(
        &self,
        req: UpdateGlobalConfigRequest,
    ) -> Result<GlobalConfigResponse, AdminServiceError> {
        // 0. 先抓写前快照：用于后续传给 setter。
        //    必须在 with_config_mut 之前抓，否则会读到闭包刚写入的新值，
        //    setter 内部 old==new 时会触发 noop（bug-B 根因）。
        let old_per_credential = self.token_manager.config().per_credential_concurrency;
        let old_global = self.token_manager.config().global_concurrency;

        // 1. 先持久化配置（失败时不影响运行时状态）
        self.token_manager.with_config_mut(|cfg| {
            if let Some(region) = &req.region {
                let trimmed = region.trim();
                if trimmed.is_empty() {
                    return Err(AdminServiceError::InvalidCredential(
                        "Region 不能为空".to_string(),
                    ));
                }
                cfg.region = trimmed.to_string();
            }

            if let Some(ttl_seconds) = req.prompt_cache_ttl_seconds {
                if !matches!(ttl_seconds, 300 | 3600) {
                    return Err(AdminServiceError::InvalidCredential(
                        "Prompt Cache TTL 仅支持 300（5分钟）或 3600（1小时）".to_string(),
                    ));
                }
                cfg.prompt_cache_ttl_seconds = ttl_seconds;
            }

            if let Some(enabled) = req.prompt_cache_accounting_enabled {
                cfg.prompt_cache_accounting_enabled = enabled;
            }

            if let Some(ref endpoint) = req.default_endpoint {
                let trimmed = endpoint.trim();
                if trimmed.is_empty() {
                    return Err(AdminServiceError::InvalidCredential(
                        "默认 endpoint 不能为空".to_string(),
                    ));
                }
                if !self.known_endpoints.contains(trimmed) {
                    let mut known: Vec<&str> =
                        self.known_endpoints.iter().map(|s| s.as_str()).collect();
                    known.sort_unstable();
                    return Err(AdminServiceError::InvalidCredential(format!(
                        "未知的 endpoint: {}，可用值: {:?}",
                        trimmed, known
                    )));
                }
                cfg.default_endpoint = trimmed.to_string();
            }

            if let Some(extract) = req.extract_thinking {
                cfg.extract_thinking = extract;
            }

            if let Some(c) = &req.compression {
                Self::apply_compression_fields(&mut cfg.compression, c);
            }

            // 凭据队列等待超时（秒）：无单独 setter，运行时 acquire 路径每次读 config 即生效
            if let Some(v) = req.acquire_wait_timeout_secs {
                cfg.acquire_wait_timeout_secs = v;
            }

            // 单凭据最大并发数 0 无意义（setter 内部也会 bail），提前拦截返回 400。
            // 注：setter 已改双参（old, new），old 由函数顶部 snapshot 传入，
            // 故此处可放心写 cfg —— setter 不再读 config，写入时机不影响 setter。
            if let Some(v) = req.per_credential_concurrency {
                if v == 0 {
                    return Err(AdminServiceError::InvalidCredential(
                        "单凭据最大并发数不能为 0".to_string(),
                    ));
                }
                cfg.per_credential_concurrency = v;
            }

            // 全局最大并发数：0 表示不限，合法
            if let Some(v) = req.global_concurrency {
                cfg.global_concurrency = v;
            }

            // 余额刷新三连：写入后统一 clamp（min 间隔 180s, 并发 1..=10）
            if let Some(v) = req.balance_refresh_enabled {
                cfg.balance_refresh_enabled = v;
            }
            if let Some(v) = req.balance_refresh_interval_secs {
                cfg.balance_refresh_interval_secs = v;
            }
            if let Some(v) = req.balance_refresh_concurrency {
                cfg.balance_refresh_concurrency = v;
            }
            cfg.clamp_balance_refresh();

            if let Some(v) = req.session_affinity_enabled {
                cfg.session_affinity_enabled = v;
            }

            if let Some(v) = req.truncation_recovery_system_notice {
                cfg.truncation_recovery_system_notice = v;
            }

            if let Some(v) = req.privacy_mode {
                cfg.privacy_mode = v;
            }

            cfg.save()
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))
        })?;

        // 2. 持久化成功后再应用运行时变更
        let config = self.token_manager.config();

        // 关闭 session 亲和后清空已有绑定，避免残留
        if let Some(false) = req.session_affinity_enabled {
            self.token_manager.clear_session_affinity();
        }

        // 截断恢复识别开关：直接同步到共享 atomic（converter 下次调用即生效）
        if let Some(v) = req.truncation_recovery_system_notice {
            self.truncation_recovery_notice
                .store(v, std::sync::atomic::Ordering::Relaxed);
        }

        // 热更新 region（注：xkiro 已剔除 credential_rpm，故不存在 update_credential_rpm 同步）
        if req.region.is_some() {
            self.token_manager.update_region(config.region.clone());
        }

        // 热更新 default_endpoint
        // 贴合 BK admin/service.rs:910-925：token_manager 先 + provider 后双层同步
        if req.default_endpoint.is_some() {
            self.token_manager
                .update_default_endpoint(config.default_endpoint.clone());
            if let Some(provider) = &self.kiro_provider {
                if let Err(e) =
                    provider.update_default_endpoint(config.default_endpoint.clone())
                {
                    tracing::warn!(
                        "provider.update_default_endpoint 失败（已持久化）: {}",
                        e
                    );
                }
            }
        }

        // 热更新 Prompt Cache 运行时配置
        if req.prompt_cache_ttl_seconds.is_some() || req.prompt_cache_accounting_enabled.is_some()
        {
            self.prompt_cache_runtime.write().update(
                req.prompt_cache_ttl_seconds,
                req.prompt_cache_accounting_enabled,
            );
        }

        // 热更新压缩配置到运行时 Arc<RwLock<CompressionConfig>>
        if let Some(c) = &req.compression {
            let mut runtime = self.compression_config.write();
            Self::apply_compression_fields(&mut runtime, c);
        }

        // 热更新单凭据最大并发数（0 不允许，setter 内部 bail）
        // old 由函数顶部 snapshot 传入，setter 内部不读 config，避免 noop。
        if let Some(v) = req.per_credential_concurrency {
            self.token_manager
                .set_per_credential_concurrency(old_per_credential, v)
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        }

        // 热更新全局并发数（0 表示不限）
        if let Some(v) = req.global_concurrency {
            self.token_manager
                .set_global_concurrency(old_global, v)
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        }

        Ok(self.get_global_config())
    }

    // ============ 系统提示注入 ============

    /// 取系统提示注入快照（含 builtin + user preset 列表 + 启用状态）
    pub fn get_system_prompt(&self) -> SystemPromptResponse {
        let rt = self.prompt_runtime.read();
        let mut presets: Vec<PresetItem> = Vec::new();

        for p in crate::anthropic::prompt_presets::PRESETS {
            let enabled = rt.enabled_presets.iter().any(|id| id == p.id);
            presets.push(PresetItem {
                id: p.id.to_string(),
                name: p.name.to_string(),
                description: p.description.to_string(),
                source: "builtin".to_string(),
                enabled,
                content: None,
            });
        }
        for up in &rt.user_presets {
            let enabled = rt.enabled_presets.iter().any(|id| id == &up.id);
            presets.push(PresetItem {
                id: up.id.clone(),
                name: up.name.clone(),
                description: up.description.clone(),
                source: "user".to_string(),
                enabled,
                content: Some(up.content.clone()),
            });
        }

        SystemPromptResponse {
            enabled: rt.enabled,
            position: match rt.position {
                SystemPromptPosition::Prepend => "prepend".to_string(),
                SystemPromptPosition::Append => "append".to_string(),
            },
            custom_content: rt.custom_content.clone(),
            presets,
        }
    }

    /// 更新系统提示注入配置（部分字段更新；持久化到 config.json）
    pub fn update_system_prompt(
        &self,
        req: UpdateSystemPromptRequest,
    ) -> Result<SystemPromptResponse, AdminServiceError> {
        let position = if let Some(pos) = req.position.as_deref() {
            match pos {
                "prepend" => Some(SystemPromptPosition::Prepend),
                "append" => Some(SystemPromptPosition::Append),
                _ => {
                    return Err(AdminServiceError::InvalidCredential(
                        "position 仅允许 'prepend' 或 'append'".to_string(),
                    ));
                }
            }
        } else {
            None
        };

        if let Some(ref ids) = req.enabled_presets {
            let user_ids: Vec<String> = self
                .prompt_runtime
                .read()
                .user_presets
                .iter()
                .map(|p| p.id.clone())
                .collect();
            for id in ids {
                let known = crate::anthropic::prompt_presets::is_builtin(id)
                    || user_ids.iter().any(|u| u == id);
                if !known {
                    return Err(AdminServiceError::InvalidCredential(format!(
                        "未知 preset id: {}",
                        id
                    )));
                }
            }
        }

        self.token_manager
            .with_config_mut(|cfg| {
                if let Some(v) = req.enabled {
                    cfg.system_prompt_enabled = v;
                }
                if let Some(p) = position {
                    cfg.system_prompt_position = p;
                }
                if let Some(c) = req.custom_content.clone() {
                    let trimmed = c.trim();
                    cfg.system_prompt = if trimmed.is_empty() {
                        None
                    } else {
                        Some(c)
                    };
                }
                if let Some(ids) = req.enabled_presets.clone() {
                    cfg.enabled_presets = ids;
                }
                cfg.save()
                    .map_err(|e| AdminServiceError::InternalError(e.to_string()))
            })?;

        // 持久化成功 → 同步运行时
        {
            let mut rt = self.prompt_runtime.write();
            if let Some(v) = req.enabled {
                rt.enabled = v;
            }
            if let Some(p) = position {
                rt.position = p;
            }
            if let Some(c) = req.custom_content {
                let trimmed = c.trim();
                rt.custom_content = if trimmed.is_empty() {
                    None
                } else {
                    Some(c)
                };
            }
            if let Some(ids) = req.enabled_presets {
                rt.enabled_presets = ids;
            }
        }

        Ok(self.get_system_prompt())
    }

    /// 新增/覆盖用户预设（id 已存在则覆盖）；不允许与内置 id 冲突
    pub fn upsert_user_preset(
        &self,
        req: UpsertUserPresetRequest,
    ) -> Result<SystemPromptResponse, AdminServiceError> {
        let id = req.id.trim().to_string();
        if id.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "preset id 不能为空".to_string(),
            ));
        }
        if id.len() > 32
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(AdminServiceError::InvalidCredential(
                "preset id 仅允许 [a-z0-9_-]，长度 1-32".to_string(),
            ));
        }
        if crate::anthropic::prompt_presets::is_builtin(&id) {
            return Err(AdminServiceError::InvalidCredential(format!(
                "id '{}' 与内置预设冲突",
                id
            )));
        }

        let preset = UserPreset {
            id: id.clone(),
            name: req.name,
            description: req.description,
            content: req.content,
        };

        self.token_manager.with_config_mut(|cfg| {
            if let Some(existing) = cfg.user_presets.iter_mut().find(|p| p.id == id) {
                *existing = preset.clone();
            } else {
                cfg.user_presets.push(preset.clone());
            }
            cfg.save()
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))
        })?;

        {
            let mut rt = self.prompt_runtime.write();
            if let Some(existing) = rt.user_presets.iter_mut().find(|p| p.id == id) {
                *existing = preset;
            } else {
                rt.user_presets.push(preset);
            }
        }

        Ok(self.get_system_prompt())
    }

    /// 删除用户预设；同时从 enabled_presets 移除
    pub fn delete_user_preset(
        &self,
        id: &str,
    ) -> Result<SystemPromptResponse, AdminServiceError> {
        let id_owned = id.to_string();

        let existed = self
            .prompt_runtime
            .read()
            .user_presets
            .iter()
            .any(|p| p.id == id_owned);
        if !existed {
            return Err(AdminServiceError::InvalidCredential(format!(
                "未找到用户预设 id: {}",
                id_owned
            )));
        }

        self.token_manager.with_config_mut(|cfg| {
            cfg.user_presets.retain(|p| p.id != id_owned);
            cfg.enabled_presets.retain(|x| x != &id_owned);
            cfg.save()
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))
        })?;

        {
            let mut rt = self.prompt_runtime.write();
            rt.user_presets.retain(|p| p.id != id_owned);
            rt.enabled_presets.retain(|x| x != &id_owned);
        }

        Ok(self.get_system_prompt())
    }

    /// 将更新请求中的压缩字段应用到目标 CompressionConfig
    ///
    /// 兼容 BK 11 字段 + xkiro 独有 5 字段（image_*  + max_request_body_bytes）。
    fn apply_compression_fields(
        target: &mut CompressionConfig,
        src: &UpdateCompressionConfigRequest,
    ) {
        if let Some(v) = src.enabled {
            target.enabled = v;
        }
        if let Some(v) = src.whitespace_compression {
            target.whitespace_compression = v;
        }
        if let Some(ref v) = src.thinking_strategy {
            target.thinking_strategy = v.clone();
        }
        if let Some(v) = src.tool_result_max_chars {
            target.tool_result_max_chars = v;
        }
        if let Some(v) = src.tool_result_head_lines {
            target.tool_result_head_lines = v;
        }
        if let Some(v) = src.tool_result_tail_lines {
            target.tool_result_tail_lines = v;
        }
        if let Some(v) = src.tool_use_input_max_chars {
            target.tool_use_input_max_chars = v;
        }
        if let Some(v) = src.tool_description_max_chars {
            target.tool_description_max_chars = v;
        }
        if let Some(v) = src.max_history_turns {
            target.max_history_turns = v;
        }
        if let Some(v) = src.max_history_chars {
            target.max_history_chars = v;
        }
        // xkiro 独有 5 字段
        if let Some(v) = src.image_max_long_edge {
            target.image_max_long_edge = v;
        }
        if let Some(v) = src.image_max_pixels_single {
            target.image_max_pixels_single = v;
        }
        if let Some(v) = src.image_max_pixels_multi {
            target.image_max_pixels_multi = v;
        }
        if let Some(v) = src.image_multi_threshold {
            target.image_multi_threshold = v;
        }
        if let Some(v) = src.image_compression_enabled {
            target.image_compression_enabled = v;
        }
        if let Some(v) = src.max_request_body_bytes {
            target.max_request_body_bytes = v;
        }
    }

    // ============ 导出 token.json ============

    /// 按 ID 列表导出凭据为 token.json 兼容格式
    ///
    /// - API Key 凭据（无 refreshToken）跳过
    /// - 不存在的 ID 跳过
    /// - 输出顺序与 `ids` 一致；可被 `import_token_json` 直接吃回
    pub fn export_credentials_to_token_json(&self, ids: &[u64]) -> Vec<ExportTokenJsonItem> {
        let creds = self.token_manager.export_credentials_by_ids(ids);
        creds
            .into_iter()
            .filter_map(|c| {
                let refresh_token = c.refresh_token.clone()?;
                if refresh_token.is_empty() {
                    return None;
                }
                let auth_method = match c.auth_method.as_deref() {
                    Some(m) => {
                        let lower = m.to_lowercase();
                        match lower.as_str() {
                            "builder-id" | "builderid" | "iam" | "idc" => "idc".to_string(),
                            "api_key" => return None, // API Key 不可导出为 token.json
                            other => other.to_string(),
                        }
                    }
                    None => "social".to_string(),
                };
                let provider = match auth_method.as_str() {
                    "idc" => "BuilderId".to_string(),
                    _ => "Social".to_string(),
                };
                Some(ExportTokenJsonItem {
                    provider,
                    refresh_token,
                    client_id: c.client_id,
                    client_secret: c.client_secret,
                    auth_method,
                    priority: c.priority,
                    region: c.region,
                    api_region: c.api_region,
                    machine_id: c.machine_id,
                })
            })
            .collect()
    }

    /// 按 ID 列表导出 KAM 兼容格式（`kiro-account-manager` 可直接 import）
    ///
    /// - API Key 凭据跳过（KAM 仅支持 OAuth）
    /// - `id` 用 UUIDv4 派生（KAM 用字符串 ID，xkiro 用 u64，需重映射避免冲突）
    /// - `label` 用 email 优先，否则用 `Kiro #{id}` 占位
    /// - `provider` 优先级：subscription_title 启发 → start_url → email 域名 → 默认
    ///   - idc + start_url 含 `awsapps.com` → `Enterprise`
    ///   - idc → `BuilderId`
    ///   - social + email 含 `gmail` → `Google`
    ///   - social + email 含 `github` → `Github`
    ///   - social → `Google`（默认）
    /// - `authMethod` 取大写 `IdC` / 小写 `social`（KAM 约定）
    /// - `addedAt` 用 RFC3339 当前时间（xkiro 不存添加时间）
    pub fn export_credentials_to_kam(&self, ids: &[u64]) -> Vec<ExportKamItem> {
        let creds = self
            .token_manager
            .export_credentials_with_state_by_ids(ids);
        let now = chrono::Local::now().to_rfc3339();
        creds
            .into_iter()
            .filter_map(|(c, enabled)| {
                let refresh_token = c.refresh_token.clone()?;
                if refresh_token.is_empty() {
                    return None;
                }
                let auth_method_lower = c
                    .auth_method
                    .as_deref()
                    .map(|m| m.to_lowercase())
                    .unwrap_or_else(|| "social".to_string());
                if auth_method_lower == "api_key" {
                    return None;
                }
                let is_idc = matches!(
                    auth_method_lower.as_str(),
                    "idc" | "builder-id" | "builderid" | "iam"
                );
                let auth_method = if is_idc {
                    "IdC".to_string()
                } else {
                    "social".to_string()
                };
                let provider = if is_idc {
                    if c.client_secret
                        .as_deref()
                        .map(|s| s.contains("awsapps.com") || s.contains("initiateLoginUri"))
                        .unwrap_or(false)
                    {
                        "Enterprise".to_string()
                    } else {
                        "BuilderId".to_string()
                    }
                } else if let Some(email) = c.email.as_deref() {
                    if email.contains("gmail") {
                        "Google".to_string()
                    } else if email.contains("github") {
                        "Github".to_string()
                    } else {
                        "Google".to_string()
                    }
                } else {
                    "Google".to_string()
                };
                let id_str = c
                    .id
                    .map(|n| format!("xkiro-{}", n))
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let label = c
                    .email
                    .clone()
                    .unwrap_or_else(|| match c.id {
                        Some(n) => format!("Kiro #{}", n),
                        None => "Kiro Account".to_string(),
                    });
                let status = if enabled { "active" } else { "disabled" };
                Some(ExportKamItem {
                    id: id_str,
                    email: c.email.clone(),
                    label,
                    status: status.to_string(),
                    added_at: now.clone(),
                    access_token: c.access_token,
                    refresh_token: Some(refresh_token),
                    expires_at: c.expires_at,
                    provider: Some(provider),
                    user_id: c.email.clone(),
                    auth_method: Some(auth_method),
                    client_id: c.client_id,
                    client_secret: c.client_secret,
                    region: c.region,
                    start_url: None,
                    profile_arn: c.profile_arn,
                    machine_id: c.machine_id,
                    enabled,
                })
            })
            .collect()
    }

    // ============ 批量导入 token.json ============

    /// 启动后台批量导入，立即返回 job_id（由 handler 传入 Arc<Self>）
    pub fn start_import_token_json(
        self: Arc<Self>,
        req: ImportTokenJsonRequest,
    ) -> StartImportJobResponse {
        let items = req.items.into_vec();
        let dry_run = req.dry_run;
        let total = items.len();

        // 生成唯一 job_id（毫秒时间戳 + 总数，足够区分同机并发批次）
        let job_id = format!(
            "import-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            total
        );

        // 初始化快照
        self.import_jobs.lock().insert(
            job_id.clone(),
            ImportJobSnapshot {
                job_id: job_id.clone(),
                status: ImportJobStatus::Running,
                total,
                done: 0,
                added: 0,
                skipped: 0,
                invalid: 0,
                error: None,
            },
        );

        let import_jobs = Arc::clone(&self.import_jobs);
        let svc = Arc::clone(&self);
        let job_id_bg = job_id.clone();

        tokio::spawn(async move {
            let result = svc
                .import_token_json_inner(items, dry_run, {
                    let import_jobs = Arc::clone(&import_jobs);
                    let jid = job_id_bg.clone();
                    move |added_d: usize, skipped_d: usize, invalid_d: usize| {
                        let mut jobs = import_jobs.lock();
                        if let Some(snap) = jobs.get_mut(&jid) {
                            snap.done += 1;
                            snap.added += added_d;
                            snap.skipped += skipped_d;
                            snap.invalid += invalid_d;
                        }
                    }
                })
                .await;

            let mut jobs = import_jobs.lock();
            if let Some(snap) = jobs.get_mut(&job_id_bg) {
                snap.status = ImportJobStatus::Done;
                snap.added = result.summary.added;
                snap.skipped = result.summary.skipped;
                snap.invalid = result.summary.invalid;
                snap.done = result.summary.parsed;
            }
        });

        StartImportJobResponse { job_id, total }
    }

    /// 查询后台导入任务进度
    pub fn get_import_job(&self, job_id: &str) -> Option<ImportJobSnapshot> {
        self.import_jobs.lock().get(job_id).cloned()
    }

    /// 批量导入内部核心逻辑（同步执行，供后台 task 和旧接口共用）
    ///
    /// 解析官方 token.json 格式，按 provider 字段自动映射 authMethod：
    /// - BuilderId/builder-id/idc → idc
    /// - Social/social → social
    async fn import_token_json_inner<F>(
        &self,
        items: Vec<TokenJsonItem>,
        dry_run: bool,
        mut on_item_done: F,
    ) -> ImportTokenJsonResponse
    where
        F: FnMut(usize, usize, usize) + Send + 'static,
    {
        use futures::stream::{self, StreamExt};

        /// 导入并发度：5 路并发刷新首验，兼顾速度与上游限流
        const IMPORT_CONCURRENCY: usize = 5;

        // 同一批次按 refreshToken 去重
        let mut seen_tokens = std::collections::HashSet::new();
        let prepared: Vec<(usize, TokenJsonItem, bool)> = items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let dup_in_batch = item
                    .refresh_token
                    .as_ref()
                    .map(|rt| !seen_tokens.insert(rt.clone()))
                    .unwrap_or(false);
                (index, item, dup_in_batch)
            })
            .collect();

        // 并发处理；结果按 index 排序还原
        let mut results: Vec<ImportItemResult> = stream::iter(prepared)
            .map(|(index, item, dup_in_batch)| async move {
                if dup_in_batch {
                    let fingerprint = Self::generate_fingerprint(&item);
                    return ImportItemResult {
                        index,
                        fingerprint,
                        action: ImportAction::Skipped,
                        reason: Some("本次导入中重复的 refreshToken".to_string()),
                        credential_id: None,
                    };
                }
                self.process_token_json_item(index, item, dry_run).await
            })
            .buffer_unordered(IMPORT_CONCURRENCY)
            .collect()
            .await;

        results.sort_by_key(|r| r.index);

        let mut added = 0usize;
        let mut skipped = 0usize;
        let mut invalid = 0usize;
        for result in &results {
            match result.action {
                ImportAction::Added => { added += 1; on_item_done(1, 0, 0); }
                ImportAction::Skipped => { skipped += 1; on_item_done(0, 1, 0); }
                ImportAction::Invalid => { invalid += 1; on_item_done(0, 0, 1); }
            }
        }

        ImportTokenJsonResponse {
            summary: ImportSummary {
                parsed: results.len(),
                added,
                skipped,
                invalid,
            },
            items: results,
        }
    }

    /// 旧接口保留：同步等待全部完成（供测试/脚本直接调用）
    pub async fn import_token_json(&self, req: ImportTokenJsonRequest) -> ImportTokenJsonResponse {
        let items = req.items.into_vec();
        self.import_token_json_inner(items, req.dry_run, |_, _, _| {}).await
    }

    /// 处理单个 token.json 项
    async fn process_token_json_item(
        &self,
        index: usize,
        item: TokenJsonItem,
        dry_run: bool,
    ) -> ImportItemResult {
        // 生成指纹（用于识别和去重）
        let fingerprint = Self::generate_fingerprint(&item);

        // 验证必填字段
        let refresh_token = match &item.refresh_token {
            Some(rt) if !rt.is_empty() => rt.clone(),
            _ => {
                return ImportItemResult {
                    index,
                    fingerprint,
                    action: ImportAction::Invalid,
                    reason: Some("缺少 refreshToken".to_string()),
                    credential_id: None,
                };
            }
        };

        // 映射 authMethod
        let auth_method = Self::map_auth_method(&item);

        // IdC 需要 clientId 和 clientSecret
        if auth_method == "idc" && (item.client_id.is_none() || item.client_secret.is_none()) {
            return ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Invalid,
                reason: Some(format!("{} 认证需要 clientId 和 clientSecret", auth_method)),
                credential_id: None,
            };
        }

        // 检查是否已存在（通过 refreshToken 前缀匹配）
        if self.token_manager.has_refresh_token_prefix(&refresh_token) {
            return ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Skipped,
                reason: Some("凭据已存在".to_string()),
                credential_id: None,
            };
        }

        // dry-run 模式只返回预览
        if dry_run {
            return ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Added,
                reason: Some("预览模式".to_string()),
                credential_id: None,
            };
        }

        // 实际添加凭据（trim + 空字符串转 None，与 set_region 逻辑一致）
        let region = item
            .region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let api_region = item
            .api_region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let new_cred = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: Some(refresh_token),
            kiro_api_key: None,
            profile_arn: None,
            expires_at: None,
            auth_method: Some(auth_method),
            client_id: item.client_id,
            client_secret: item.client_secret,
            priority: item.priority,
            region,
            auth_region: None,
            api_region,
            machine_id: item.machine_id,
            endpoint: None,
            email: None,
            subscription_title: None,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            proxy_id: None,
            disabled: false,
            concurrency: None,
            group: None,
            source: None,
        };

        match self.token_manager.add_credential(new_cred, false).await {
            Ok(credential_id) => {
                // validate=false：首刷失败也会入池为禁用态，交给后台定时重试。
                // 这里据实告知用户该号是「已激活」还是「已入池待自动重试」。
                let pending_retry = self.token_manager.is_credential_disabled(credential_id);
                // 导入即默认开启超额(overage)。仅对激活成功的号尝试;失败不影响导入结果
                // (上游开关失败时号照常可用,用户可后续手动开)。禁用态的号留给后台重试激活后再说。
                if !pending_retry {
                    if let Err(e) = self
                        .token_manager
                        .set_overage_status_for(credential_id, true)
                        .await
                    {
                        tracing::warn!("凭据 #{} 导入后默认开启超额失败(不影响导入): {}", credential_id, e);
                    }
                }
                ImportItemResult {
                    index,
                    fingerprint,
                    action: ImportAction::Added,
                    reason: if pending_retry {
                        Some("已入池，首次验证失败，将自动重试".to_string())
                    } else {
                        None
                    },
                    credential_id: Some(credential_id),
                }
            }
            Err(e) => ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Invalid,
                reason: Some(e.to_string()),
                credential_id: None,
            },
        }
    }

    /// 生成凭据指纹（用于识别）
    ///
    /// 使用 refreshToken 前 16 字符作为指纹，floor_char_boundary 安全截断
    fn generate_fingerprint(item: &TokenJsonItem) -> String {
        item.refresh_token
            .as_ref()
            .map(|rt| {
                if rt.len() >= 16 {
                    let end = floor_char_boundary(rt, 16);
                    format!("{}...", &rt[..end])
                } else {
                    rt.clone()
                }
            })
            .unwrap_or_else(|| "(empty)".to_string())
    }

    /// 映射 provider/authMethod 到标准 authMethod
    ///
    /// 优先级：authMethod > provider > 默认 social
    fn map_auth_method(item: &TokenJsonItem) -> String {
        // 优先使用 authMethod 字段
        if let Some(auth) = &item.auth_method {
            let auth_lower = auth.to_lowercase();
            return match auth_lower.as_str() {
                "idc" | "builder-id" | "builderid" => "idc".to_string(),
                "social" => "social".to_string(),
                _ => auth_lower,
            };
        }

        // 回退到 provider 字段
        if let Some(provider) = &item.provider {
            let provider_lower = provider.to_lowercase();
            return match provider_lower.as_str() {
                "builderid" | "builder-id" | "idc" => "idc".to_string(),
                "social" => "social".to_string(),
                _ => "social".to_string(),
            };
        }

        // 默认 social
        "social".to_string()
    }

    // ========================================================
    // 代理池(引用式绑定)管理
    // ========================================================

    /// 列出所有代理(含运行时健康 + 可用 permit + 绑定该代理的号数)
    pub fn list_proxies(&self) -> serde_json::Value {
        let views = self.proxy_manager.list();
        let items: Vec<serde_json::Value> = views
            .into_iter()
            .map(|v| {
                let id = v.entry.id.unwrap_or(0);
                let bound = self.token_manager.credentials_bound_to_proxy(id).len();
                serde_json::json!({
                    "id": id,
                    "url": v.entry.url,
                    "username": v.entry.username,
                    "region": v.entry.region,
                    "country": v.entry.country,
                    "maxConcurrency": v.entry.max_concurrency,
                    "disabled": v.entry.disabled,
                    "note": v.entry.note,
                    "dead": v.health.dead,
                    "consecutiveFailures": v.health.consecutive_failures,
                    "lastError": v.health.last_error,
                    "lastChecked": v.health.last_checked,
                    "availablePermits": v.available_permits,
                    "boundCredentials": bound,
                })
            })
            .collect();
        serde_json::json!({ "proxies": items })
    }

    /// 新增代理
    pub fn add_proxy(&self, req: ProxyUpsertRequest) -> Result<u64, AdminServiceError> {
        let entry = Self::proxy_entry_from_req(req);
        self.proxy_manager
            .add(entry)
            .map_err(|e| AdminServiceError::InternalError(format!("新增代理失败: {}", e)))
    }

    /// 更新代理
    pub fn update_proxy(&self, id: u64, req: ProxyUpsertRequest) -> Result<(), AdminServiceError> {
        let entry = Self::proxy_entry_from_req(req);
        self.proxy_manager
            .update(id, entry)
            .map_err(|e| AdminServiceError::InternalError(format!("更新代理失败: {}", e)))
    }

    /// 删除代理(先解绑所有引用它的号,避免悬空 proxy_id)
    pub fn delete_proxy(&self, id: u64) -> Result<usize, AdminServiceError> {
        let bound = self.token_manager.credentials_bound_to_proxy(id);
        for cid in &bound {
            // 解绑失败不致命,记录后继续(代理删了,号会降级到无代理)
            if let Err(e) = self.token_manager.set_proxy_id(*cid, None) {
                tracing::warn!("删除代理 #{} 时解绑号 #{} 失败: {}", id, cid, e);
            }
        }
        self.proxy_manager
            .delete(id)
            .map_err(|e| AdminServiceError::InternalError(format!("删除代理失败: {}", e)))?;
        Ok(bound.len())
    }

    /// 给单个号绑定/解绑代理
    pub fn set_credential_proxy(
        &self,
        cred_id: u64,
        proxy_id: Option<u64>,
    ) -> Result<(), AdminServiceError> {
        // 绑定时校验代理存在
        if let Some(pid) = proxy_id {
            if self.proxy_manager.get(pid).is_none() {
                return Err(AdminServiceError::InvalidCredential(format!(
                    "代理 #{} 不存在",
                    pid
                )));
            }
        }
        self.token_manager
            .set_proxy_id(cred_id, proxy_id)
            .map_err(|e| AdminServiceError::InternalError(format!("设置号代理绑定失败: {}", e)))
    }

    /// 测试单个代理连通性(经代理请求出口 IP 探测端点)
    pub async fn test_proxy(&self, id: u64) -> ProxyTestResponse {
        let entry = match self.proxy_manager.get(id) {
            Some(e) => e,
            None => {
                return ProxyTestResponse {
                    ok: false,
                    exit_ip: None,
                    latency_ms: None,
                    error: Some(format!("代理 #{} 不存在", id)),
                };
            }
        };
        let result = Self::probe_proxy(&entry).await;
        // 把测试结果计入健康状态(成功清零失败计数,失败累加→可能判 dead)
        self.proxy_manager
            .record_health(id, result.ok, result.error.clone());
        result
    }

    /// 批量导入代理:每行 `url` 或 `url,user,pass`,以 `#` 开头或空行跳过
    /// 批量导入代理。每行支持两种格式(自动识别):
    ///   - `ip:port:username:password` 或 `ip:port`(冒号分隔,默认 socks5://)
    ///   - `url[,username[,password]]`(逗号分隔,url 自带 scheme)
    /// 导入后逐个经代理探测 ipinfo.io/json 回填 region/country(失败不影响导入)。
    /// `#` 开头或空行跳过。
    pub async fn import_proxies(&self, req: ProxyImportRequest) -> ProxyImportResponse {
        let mut added = 0usize;
        let mut failed = 0usize;
        let mut errors = Vec::new();
        let mut new_ids: Vec<u64> = Vec::new();
        for (lineno, raw) in req.text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (url, username, password) = match parse_proxy_line(line) {
                Ok(v) => v,
                Err(e) => {
                    failed += 1;
                    errors.push(format!("第{}行: {}", lineno + 1, e));
                    continue;
                }
            };
            let entry = crate::kiro::proxy_manager::ProxyEntry {
                id: None,
                url,
                username,
                password,
                region: req.region.clone(),
                country: None,
                max_concurrency: req.max_concurrency,
                disabled: false,
                note: None,
            };
            match self.proxy_manager.add(entry) {
                Ok(id) => {
                    added += 1;
                    new_ids.push(id);
                }
                Err(e) => {
                    failed += 1;
                    errors.push(format!("第{}行: {}", lineno + 1, e));
                }
            }
        }

        // 导入完成后,逐个经代理探测出口地理(region/country)。这步是 best-effort:
        // 探测失败仅跳过该条的 geo 回填,不计入 failed、不影响导入结果。
        for id in new_ids {
            if let Some(entry) = self.proxy_manager.get(id) {
                if let Some(geo) = probe_proxy_geo(&entry).await {
                    // 行内/批量已显式指定 region 时,不用 ipinfo 覆盖;country 始终回填
                    let region = if entry.region.is_some() { None } else { geo.region };
                    if let Err(e) = self.proxy_manager.set_geo(id, region, geo.country) {
                        tracing::warn!("代理 #{} 回填地理信息失败: {}", id, e);
                    }
                }
            }
        }

        ProxyImportResponse {
            added,
            failed,
            errors,
        }
    }

    /// 自动分配:给未绑定(或强制重分)的号按 region 匹配可用代理
    ///
    /// 匹配规则:号的 region 与代理 region 相同优先;代理无 region 视为通配兜底。
    /// 同一 region 内多代理时按"当前绑定数最少"均摊。
    pub fn auto_assign_proxies(&self, req: ProxyAutoAssignRequest) -> ProxyAutoAssignResponse {
        let bindings = self.token_manager.credential_region_bindings();
        // 统计每个代理当前绑定数,用于均摊
        let mut load: HashMap<u64, usize> = HashMap::new();
        for (_, _, pid, _) in &bindings {
            if let Some(p) = pid {
                *load.entry(*p).or_insert(0) += 1;
            }
        }
        let proxies = self.proxy_manager.list();

        let mut assigned = Vec::new();
        let mut skipped = Vec::new();

        for (cid, region, cur_proxy, disabled) in bindings {
            // 过滤:指定了 id 列表则只处理列表内;禁用号跳过
            if !req.credential_ids.is_empty() && !req.credential_ids.contains(&cid) {
                continue;
            }
            if disabled {
                continue;
            }
            // 已绑定且不强制重分 → 跳过(不计入 skipped,本就不需处理)
            if cur_proxy.is_some() && !req.reassign_bound {
                continue;
            }

            // 候选:启用 + 未 dead;region 匹配(相同 region) 或代理无 region(通配)
            let mut candidates: Vec<u64> = proxies
                .iter()
                .filter(|v| !v.entry.disabled && !v.health.dead)
                .filter(|v| match (&region, &v.entry.region) {
                    (Some(cr), Some(pr)) => cr == pr,
                    (_, None) => true, // 代理无 region = 通配兜底
                    (None, Some(_)) => false, // 号无 region 不配给特定 region 代理
                })
                .filter_map(|v| v.entry.id)
                .collect();

            // 按当前负载升序,均摊分配
            candidates.sort_by_key(|p| *load.get(p).unwrap_or(&0));

            match candidates.first() {
                Some(&pid) => {
                    if let Err(e) = self.token_manager.set_proxy_id(cid, Some(pid)) {
                        tracing::warn!("自动分配:号 #{} 绑代理 #{} 失败: {}", cid, pid, e);
                        skipped.push(cid);
                    } else {
                        *load.entry(pid).or_insert(0) += 1;
                        assigned.push((cid, pid));
                    }
                }
                None => skipped.push(cid),
            }
        }

        ProxyAutoAssignResponse { assigned, skipped }
    }

    /// ProxyUpsertRequest → ProxyEntry(id 留空由 manager 分配)
    fn proxy_entry_from_req(req: ProxyUpsertRequest) -> crate::kiro::proxy_manager::ProxyEntry {
        crate::kiro::proxy_manager::ProxyEntry {
            id: None,
            url: req.url,
            username: req.username,
            password: req.password,
            region: req.region,
            country: None,
            max_concurrency: req.max_concurrency,
            disabled: req.disabled,
            note: req.note,
        }
    }

    /// 经代理探测出口 IP(连通性测试)。10s 超时,Rustls 后端。
    async fn probe_proxy(entry: &crate::kiro::proxy_manager::ProxyEntry) -> ProxyTestResponse {
        let proxy_cfg = entry.to_proxy_config();
        let client = match crate::http_client::build_client(
            Some(&proxy_cfg),
            10,
            crate::model::config::TlsBackend::Rustls,
        ) {
            Ok(c) => c,
            Err(e) => {
                return ProxyTestResponse {
                    ok: false,
                    exit_ip: None,
                    latency_ms: None,
                    error: Some(format!("构建代理客户端失败: {}", e)),
                };
            }
        };

        let start = std::time::Instant::now();
        // 用轻量的出口 IP 探测端点
        let resp = client.get("https://api.ipify.org?format=text").send().await;
        let latency_ms = start.elapsed().as_millis() as u64;

        match resp {
            Ok(r) if r.status().is_success() => {
                let exit_ip = r.text().await.ok().map(|s| s.trim().to_string());
                ProxyTestResponse {
                    ok: true,
                    exit_ip,
                    latency_ms: Some(latency_ms),
                    error: None,
                }
            }
            Ok(r) => ProxyTestResponse {
                ok: false,
                exit_ip: None,
                latency_ms: Some(latency_ms),
                error: Some(format!("探测端点返回 HTTP {}", r.status())),
            },
            Err(e) => ProxyTestResponse {
                ok: false,
                exit_ip: None,
                latency_ms: Some(latency_ms),
                error: Some(format!("代理连接失败: {}", e)),
            },
        }
    }

    // ============ Dashboard 聚合 ============

    /// 总览 KPI（1h / 24h 双窗口）
    pub fn dashboard_overview(&self) -> crate::admin::metrics::DashboardOverview {
        let now = chrono::Utc::now().timestamp();
        self.metrics.overview(now)
    }

    /// 时序折线数据（window_minutes=60, interval_minutes=5 → 12 个点）
    pub fn dashboard_series(
        &self,
        window_minutes: u64,
        interval_minutes: u64,
    ) -> Vec<crate::admin::metrics::SeriesBucket> {
        let now = chrono::Utc::now().timestamp();
        self.metrics.series(now, window_minutes, interval_minutes)
    }

    // ============ API Key 管理 ============

    /// key → 稳定 id（sha256 前 12 位 hex）
    fn api_key_id(key: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(key.as_bytes());
        hex::encode(h.finalize())[..12].to_string()
    }

    /// 脱敏展示：保留前 11 位（sk-xkiro- 前缀区）与后 4 位
    fn mask_api_key(key: &str) -> String {
        let n = key.chars().count();
        if n <= 12 {
            // 太短：只露后两位
            let tail: String = key.chars().rev().take(2).collect::<Vec<_>>().into_iter().rev().collect();
            return format!("…{}", tail);
        }
        let head: String = key.chars().take(11).collect();
        let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        format!("{}…{}", head, tail)
    }

    /// 列出全部 API Key（脱敏）
    pub fn list_api_keys(&self) -> Vec<crate::admin::types::ApiKeyItem> {
        self.api_keys
            .read()
            .iter()
            .map(|e| crate::admin::types::ApiKeyItem {
                masked: Self::mask_api_key(&e.key),
                id: Self::api_key_id(&e.key),
                group: e.group.clone(),
            })
            .collect()
    }

    /// 新增 API Key；key 留空则自动生成 sk-xkiro-<32hex>
    pub fn add_api_key(
        &self,
        req: crate::admin::types::CreateApiKeyRequest,
    ) -> anyhow::Result<crate::admin::types::CreateApiKeyResponse> {
        let key = req
            .key
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| format!("sk-xkiro-{}", uuid::Uuid::new_v4().simple()));
        let group = req
            .group
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty());

        {
            let mut keys = self.api_keys.write();
            if keys.iter().any(|e| e.key == key) {
                anyhow::bail!("该 API Key 已存在");
            }
            keys.push(crate::model::config::ApiKeyEntry {
                key: key.clone(),
                group: group.clone(),
            });
        }
        self.persist_api_keys()?;
        Ok(crate::admin::types::CreateApiKeyResponse { key, group })
    }

    /// 按 id（sha256 前缀）删除 API Key
    pub fn delete_api_key(&self, id: &str) -> anyhow::Result<()> {
        let before = self.api_keys.read().len();
        self.api_keys
            .write()
            .retain(|e| Self::api_key_id(&e.key) != id);
        let after = self.api_keys.read().len();
        if before == after {
            anyhow::bail!("未找到对应 API Key");
        }
        self.persist_api_keys()?;
        Ok(())
    }

    /// 把当前内存中的 api_keys 写回 config.json（重载后覆盖 api_keys 字段再原子保存）
    fn persist_api_keys(&self) -> anyhow::Result<()> {
        let Some(path) = self.config_path.as_ref() else {
            tracing::warn!("config_path 未知，API Key 变更仅在内存生效，重启后丢失");
            return Ok(());
        };
        let mut config = crate::model::config::Config::load(path)
            .with_context(|| format!("重载配置失败: {}", path.display()))?;
        config.api_keys = self.api_keys.read().clone();
        config.save().context("保存 config.json 失败")?;
        Ok(())
    }
}

/// ipinfo.io/json 探测到的出口地理信息(仅取关心的两个字段)
struct ProxyGeo {
    region: Option<String>,
    country: Option<String>,
}

/// 解析一行代理文本,返回 (url, username, password)。支持两种格式:
///   1. 冒号分隔 `ip:port[:user:pass]` —— 无 scheme,默认补 `socks5://`
///   2. 逗号分隔 `url[,user[,pass]]` —— url 自带 scheme(http/https/socks5(h))
/// 识别规则:含 `://` 或含 `,` → 走逗号格式;否则按冒号格式拆。
fn parse_proxy_line(line: &str) -> Result<(String, Option<String>, Option<String>), String> {
    let line = line.trim();
    // 逗号格式(原有):显式 url[,user[,pass]]
    if line.contains("://") || line.contains(',') {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        let url = parts[0].to_string();
        if url.is_empty() {
            return Err("url 为空".to_string());
        }
        let username = parts.get(1).filter(|s| !s.is_empty()).map(|s| s.to_string());
        let password = parts.get(2).filter(|s| !s.is_empty()).map(|s| s.to_string());
        return Ok((url, username, password));
    }
    // 冒号格式:ip:port 或 ip:port:user:pass,默认 socks5://
    let parts: Vec<&str> = line.split(':').map(|s| s.trim()).collect();
    match parts.as_slice() {
        [ip, port] => {
            if ip.is_empty() || port.is_empty() {
                return Err(format!("冒号格式需 ip:port,得到: {}", line));
            }
            Ok((format!("socks5://{}:{}", ip, port), None, None))
        }
        [ip, port, user, pass] => {
            if ip.is_empty() || port.is_empty() {
                return Err(format!("冒号格式需 ip:port:user:pass,得到: {}", line));
            }
            let username = (!user.is_empty()).then(|| user.to_string());
            let password = (!pass.is_empty()).then(|| pass.to_string());
            Ok((format!("socks5://{}:{}", ip, port), username, password))
        }
        _ => Err(format!(
            "无法识别的代理格式(支持 ip:port[:user:pass] 或 url[,user,pass]): {}",
            line
        )),
    }
}

/// 经代理请求 ipinfo.io/json,拿出口的 region/country。失败返回 None(best-effort)。
async fn probe_proxy_geo(entry: &crate::kiro::proxy_manager::ProxyEntry) -> Option<ProxyGeo> {
    let proxy_cfg = entry.to_proxy_config();
    let client = crate::http_client::build_client(
        Some(&proxy_cfg),
        10,
        crate::model::config::TlsBackend::Rustls,
    )
    .ok()?;
    let resp = client.get("https://ipinfo.io/json").send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    let region = json
        .get("region")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let country = json
        .get("country")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if region.is_none() && country.is_none() {
        return None;
    }
    Some(ProxyGeo { region, country })
}

use crate::kiro::token_manager::CreditUsageObserver;

impl CreditUsageObserver for AdminService {
    fn on_credit_usage(
        &self,
        id: u64,
        credit: f64,
        new_primary_remaining: f64,
        new_overage_remaining: f64,
    ) {
        if !credit.is_finite() || credit <= 0.0 {
            return;
        }

        let mutated = {
            let mut cache = self.balance_cache.lock();
            let Some(entry) = cache.get_mut(&id) else {
                return;
            };
            let data = &mut entry.data;
            data.current_usage = (data.current_usage + credit).max(0.0);
            data.remaining = new_primary_remaining;
            data.usage_percentage = if data.usage_limit > 0.0 {
                ((data.current_usage / data.usage_limit) * 100.0).min(9_999.0)
            } else {
                0.0
            };
            entry.cached_at = Utc::now().timestamp() as f64;
            tracing::debug!(
                credential_id = id,
                credit,
                new_remaining = data.remaining,
                new_overage = new_overage_remaining,
                "AdminService disk cache 已同步 metering 扣减"
            );
            true
        };

        if mutated {
            self.save_balance_cache();
        }
    }
}
