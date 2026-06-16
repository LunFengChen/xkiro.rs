//! 代理池管理器
//!
//! 管理一批 HTTP/SOCKS 代理（独立于凭据),持久化到 `proxies.json`。
//! 号(凭据)通过 `proxy_id` 引用式绑定到代理;运行时由 token_manager 在
//! `acquire_context` 里按 id 查出真实 url 临时回填进 credentials 副本,
//! provider 照旧按 effective_proxy 命中 Client 缓存,底层无需改动。
//!
//! 设计要点:
//! - 持久化字段(url/账密/region/max_concurrency/disabled/note)落盘 proxies.json
//! - 运行时字段(health: 连续失败/dead/最近检测)**不落盘**,重启默认 alive 重新巡检
//! - 每代理一把信号量(仅 max_concurrency>0 时建),实现"每代理并发上限"
//! - 原子写复用 common::io::atomic_write_string_secure(tmp+rename+chmod600)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::http_client::ProxyConfig;

/// 代理池条目(持久化部分)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEntry {
    /// 代理唯一 ID(自增,缺失时由 ProxyManager 补全)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,

    /// 代理地址,支持 http/https/socks5(h)
    pub url: String,

    /// 代理认证用户名(可选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// 代理认证密码(可选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,

    /// 该代理出口对应的区域(用于 region 感知分配)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,

    /// 每代理并发上限。None 或 0 视为不限(不建信号量,调度时直接放行)
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,

    /// 是否禁用(默认 false=启用,与 KiroCredentials.disabled 语义一致)
    #[serde(default)]
    pub disabled: bool,

    /// 备注
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ProxyEntry {
    /// 默认代理池文件名(与 credentials.json 同目录)
    pub fn default_proxies_path() -> &'static str {
        "proxies.json"
    }

    /// 转换为 http_client 的 ProxyConfig(用于构建 Client / 连通测试)
    pub fn to_proxy_config(&self) -> ProxyConfig {
        let mut cfg = ProxyConfig::new(&self.url);
        if let (Some(u), Some(p)) = (&self.username, &self.password) {
            cfg = cfg.with_auth(u, p);
        }
        cfg
    }
}

/// 代理运行时健康状态(不落盘)
#[derive(Debug, Clone, Default)]
pub struct ProxyHealth {
    /// 连续连通失败次数
    pub consecutive_failures: u32,
    /// 是否已判定为 dead(踢出调度)
    pub dead: bool,
    /// 最近一次连通检测时间
    pub last_checked: Option<DateTime<Utc>>,
    /// 最近一次失败原因
    pub last_error: Option<String>,
}

/// 单个代理的运行时状态(持久化条目 + 健康)
struct ProxyState {
    entry: ProxyEntry,
    health: ProxyHealth,
}

/// 连续失败多少次判定代理 dead
pub const PROXY_DEAD_THRESHOLD: u32 = 3;

/// 代理池管理器
pub struct ProxyManager {
    /// 代理条目 + 健康状态(按插入顺序)
    states: Mutex<Vec<ProxyState>>,
    /// 每代理并发信号量(仅 max_concurrency>0 的代理建)
    semaphores: Mutex<HashMap<u64, Arc<Semaphore>>>,
    /// proxies.json 路径(None 表示不持久化,如测试场景)
    proxies_path: Option<PathBuf>,
}

impl ProxyManager {
    /// 从代理列表构造管理器
    ///
    /// - 补全缺失的自增 id(若有补全则立即回写一次)
    /// - 为 max_concurrency>0 的代理建信号量
    pub fn new(proxies: Vec<ProxyEntry>, proxies_path: Option<PathBuf>) -> anyhow::Result<Self> {
        let max_existing_id = proxies.iter().filter_map(|p| p.id).max().unwrap_or(0);
        let mut next_id = max_existing_id + 1;
        let mut has_new_ids = false;

        let mut states: Vec<ProxyState> = Vec::with_capacity(proxies.len());
        let mut seen_ids = std::collections::HashSet::new();
        for mut entry in proxies {
            let id = entry.id.unwrap_or_else(|| {
                let id = next_id;
                next_id += 1;
                entry.id = Some(id);
                has_new_ids = true;
                id
            });
            if !seen_ids.insert(id) {
                anyhow::bail!("检测到重复的代理 ID: {}", id);
            }
            states.push(ProxyState {
                entry,
                health: ProxyHealth::default(),
            });
        }

        let semaphores = Self::build_semaphores(&states);

        let manager = Self {
            states: Mutex::new(states),
            semaphores: Mutex::new(semaphores),
            proxies_path,
        };

        // 启动补全了 id → 立即回写,保证 id 稳定
        if has_new_ids {
            if let Err(e) = manager.persist() {
                tracing::warn!("代理池启动回写失败(已补全 id): {}", e);
            }
        }

        Ok(manager)
    }

    /// 从文件加载并构造(文件不存在/为空 → 空池)
    pub fn load_from(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let proxies = Self::read_file(path)?;
        Self::new(proxies, Some(path.to_path_buf()))
    }

    /// 读 proxies.json(文件不存在/空 → 空数组)
    fn read_file(path: &Path) -> anyhow::Result<Vec<ProxyEntry>> {
        if !path.exists() {
            return Ok(vec![]);
        }
        let content = std::fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(vec![]);
        }
        let proxies: Vec<ProxyEntry> = serde_json::from_str(&content)?;
        Ok(proxies)
    }

    /// 为有并发上限的代理建信号量
    fn build_semaphores(states: &[ProxyState]) -> HashMap<u64, Arc<Semaphore>> {
        states
            .iter()
            .filter_map(|s| {
                let id = s.entry.id?;
                match s.entry.max_concurrency {
                    Some(n) if n > 0 => Some((id, Arc::new(Semaphore::new(n as usize)))),
                    _ => None, // None/0 = 不限,不建信号量
                }
            })
            .collect()
    }

    /// 持久化到 proxies.json(原子写 + chmod600)。无路径则跳过。
    fn persist(&self) -> anyhow::Result<bool> {
        use anyhow::Context;

        let path = match &self.proxies_path {
            Some(p) => p,
            None => return Ok(false),
        };

        let entries: Vec<ProxyEntry> = {
            let states = self.states.lock();
            states.iter().map(|s| s.entry.clone()).collect()
        };

        let json = serde_json::to_string_pretty(&entries).context("序列化代理池失败")?;
        let real_path = crate::common::io::resolve_symlink_target(path);

        let do_write = || -> anyhow::Result<()> {
            crate::common::io::atomic_write_string_secure(&real_path, &json)
                .with_context(|| format!("原子写入代理池文件失败: {:?}", real_path))
        };

        // Tokio runtime 内用 block_in_place 避免阻塞 worker
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(do_write)?;
        } else {
            do_write()?;
        }

        tracing::debug!("已回写代理池到文件: {:?}", path);
        Ok(true)
    }

    /// 列出所有代理(克隆条目 + 健康快照,供 DTO 组装)
    pub fn list(&self) -> Vec<ProxyView> {
        let states = self.states.lock();
        let sems = self.semaphores.lock();
        states
            .iter()
            .map(|s| {
                let available = s
                    .entry
                    .id
                    .and_then(|id| sems.get(&id))
                    .map(|sem| sem.available_permits());
                ProxyView {
                    entry: s.entry.clone(),
                    health: s.health.clone(),
                    available_permits: available,
                }
            })
            .collect()
    }

    /// 按 id 查代理条目(克隆)
    pub fn get(&self, id: u64) -> Option<ProxyEntry> {
        let states = self.states.lock();
        states
            .iter()
            .find(|s| s.entry.id == Some(id))
            .map(|s| s.entry.clone())
    }

    /// 按 id 取代理的信号量(若有并发上限)
    pub fn semaphore_for(&self, id: u64) -> Option<Arc<Semaphore>> {
        self.semaphores.lock().get(&id).cloned()
    }

    /// 代理是否可用于调度(存在、未禁用、未 dead)
    pub fn is_usable(&self, id: u64) -> bool {
        let states = self.states.lock();
        states
            .iter()
            .find(|s| s.entry.id == Some(id))
            .map(|s| !s.entry.disabled && !s.health.dead)
            .unwrap_or(false)
    }

    /// 新增代理,返回分配的 id
    pub fn add(&self, mut entry: ProxyEntry) -> anyhow::Result<u64> {
        let id = {
            let mut states = self.states.lock();
            let next_id = states
                .iter()
                .filter_map(|s| s.entry.id)
                .max()
                .unwrap_or(0)
                + 1;
            entry.id = Some(next_id);
            // 同步信号量
            if let Some(n) = entry.max_concurrency {
                if n > 0 {
                    self.semaphores
                        .lock()
                        .insert(next_id, Arc::new(Semaphore::new(n as usize)));
                }
            }
            states.push(ProxyState {
                entry,
                health: ProxyHealth::default(),
            });
            next_id
        };
        self.persist()?;
        Ok(id)
    }

    /// 更新代理(整体替换持久化字段;并发上限变化时重建信号量)
    pub fn update(&self, id: u64, mut updated: ProxyEntry) -> anyhow::Result<()> {
        {
            let mut states = self.states.lock();
            let state = states
                .iter_mut()
                .find(|s| s.entry.id == Some(id))
                .ok_or_else(|| anyhow::anyhow!("代理 #{} 不存在", id))?;
            updated.id = Some(id);
            let new_limit = updated.max_concurrency;
            state.entry = updated;
            // 重建该代理信号量
            let mut sems = self.semaphores.lock();
            match new_limit {
                Some(n) if n > 0 => {
                    sems.insert(id, Arc::new(Semaphore::new(n as usize)));
                }
                _ => {
                    sems.remove(&id);
                }
            }
        }
        self.persist()?;
        Ok(())
    }

    /// 删除代理
    pub fn delete(&self, id: u64) -> anyhow::Result<()> {
        {
            let mut states = self.states.lock();
            let before = states.len();
            states.retain(|s| s.entry.id != Some(id));
            if states.len() == before {
                anyhow::bail!("代理 #{} 不存在", id);
            }
            self.semaphores.lock().remove(&id);
        }
        self.persist()?;
        Ok(())
    }

    /// 记录一次连通检测结果(供健康巡检调用)。
    /// 返回 (是否刚刚被判 dead, 是否刚刚恢复)。
    pub fn record_health(&self, id: u64, ok: bool, error: Option<String>) -> (bool, bool) {
        let mut states = self.states.lock();
        let state = match states.iter_mut().find(|s| s.entry.id == Some(id)) {
            Some(s) => s,
            None => return (false, false),
        };
        state.health.last_checked = Some(Utc::now());
        let was_dead = state.health.dead;
        if ok {
            state.health.consecutive_failures = 0;
            state.health.last_error = None;
            state.health.dead = false;
            (false, was_dead) // 刚恢复 = 之前 dead 且现在 ok
        } else {
            state.health.consecutive_failures += 1;
            state.health.last_error = error;
            let now_dead = state.health.consecutive_failures >= PROXY_DEAD_THRESHOLD;
            state.health.dead = now_dead;
            (now_dead && !was_dead, false) // 刚判 dead = 之前不 dead 且现在 dead
        }
    }

    /// 取所有启用的代理 id(供巡检遍历)
    pub fn enabled_ids(&self) -> Vec<u64> {
        let states = self.states.lock();
        states
            .iter()
            .filter(|s| !s.entry.disabled)
            .filter_map(|s| s.entry.id)
            .collect()
    }

    /// 为指定 region 挑一个可用代理(未禁用、未 dead),优先剩余并发多的。
    /// region 为 None 时匹配任意可用代理。返回代理 id。
    pub fn pick_usable_for_region(&self, region: Option<&str>) -> Option<u64> {
        let states = self.states.lock();
        let sems = self.semaphores.lock();
        states
            .iter()
            .filter(|s| !s.entry.disabled && !s.health.dead)
            .filter(|s| match region {
                Some(r) => s.entry.region.as_deref() == Some(r),
                None => true,
            })
            .max_by_key(|s| {
                // 剩余并发多的优先;无信号量(不限)视为很大
                s.entry
                    .id
                    .and_then(|id| sems.get(&id))
                    .map(|sem| sem.available_permits())
                    .unwrap_or(usize::MAX)
            })
            .and_then(|s| s.entry.id)
    }
}

/// 代理视图(条目 + 健康 + 实时可用并发),用于 admin DTO 组装
#[derive(Debug, Clone)]
pub struct ProxyView {
    pub entry: ProxyEntry,
    pub health: ProxyHealth,
    pub available_permits: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(url: &str, region: Option<&str>, max_c: Option<u32>) -> ProxyEntry {
        ProxyEntry {
            url: url.to_string(),
            region: region.map(|s| s.to_string()),
            max_concurrency: max_c,
            ..Default::default()
        }
    }

    #[test]
    fn new_assigns_incremental_ids() {
        let mgr = ProxyManager::new(
            vec![entry("http://a", None, None), entry("http://b", None, None)],
            None,
        )
        .unwrap();
        let list = mgr.list();
        assert_eq!(list[0].entry.id, Some(1));
        assert_eq!(list[1].entry.id, Some(2));
    }

    #[test]
    fn new_preserves_existing_ids() {
        let mut e = entry("http://a", None, None);
        e.id = Some(5);
        let mgr = ProxyManager::new(vec![e, entry("http://b", None, None)], None).unwrap();
        let list = mgr.list();
        assert_eq!(list[0].entry.id, Some(5));
        assert_eq!(list[1].entry.id, Some(6)); // 接着最大 id 自增
    }

    #[test]
    fn semaphore_only_for_positive_limit() {
        let mgr = ProxyManager::new(
            vec![
                entry("http://limited", None, Some(2)),
                entry("http://unlimited", None, None),
                entry("http://zero", None, Some(0)),
            ],
            None,
        )
        .unwrap();
        // id 1 有信号量(2 permit),id 2/3 无
        assert!(mgr.semaphore_for(1).is_some());
        assert_eq!(mgr.semaphore_for(1).unwrap().available_permits(), 2);
        assert!(mgr.semaphore_for(2).is_none());
        assert!(mgr.semaphore_for(3).is_none());
    }

    #[test]
    fn read_file_missing_returns_empty() {
        let p = std::env::temp_dir().join(format!("xkiro-proxytest-missing-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let proxies = ProxyManager::read_file(&p).unwrap();
        assert!(proxies.is_empty());
    }

    #[test]
    fn add_update_delete_roundtrip() {
        let mgr = ProxyManager::new(vec![], None).unwrap();
        let id = mgr.add(entry("http://x", Some("us"), Some(1))).unwrap();
        assert_eq!(id, 1);
        assert!(mgr.semaphore_for(id).is_some());

        // 更新为不限并发 → 信号量移除
        mgr.update(id, entry("http://x", Some("us"), None)).unwrap();
        assert!(mgr.semaphore_for(id).is_none());

        mgr.delete(id).unwrap();
        assert!(mgr.get(id).is_none());
        assert!(mgr.delete(id).is_err()); // 再删报错
    }

    #[test]
    fn health_marks_dead_after_threshold_and_recovers() {
        let mgr = ProxyManager::new(vec![entry("http://x", None, None)], None).unwrap();
        let id = 1;
        assert!(mgr.is_usable(id));

        // 连续失败到阈值
        for i in 1..PROXY_DEAD_THRESHOLD {
            let (now_dead, _) = mgr.record_health(id, false, Some("timeout".into()));
            assert!(!now_dead, "第 {} 次失败不该判 dead", i);
        }
        let (now_dead, _) = mgr.record_health(id, false, Some("timeout".into()));
        assert!(now_dead, "达到阈值应判 dead");
        assert!(!mgr.is_usable(id));

        // 一次成功即恢复
        let (_, recovered) = mgr.record_health(id, true, None);
        assert!(recovered);
        assert!(mgr.is_usable(id));
    }

    #[test]
    fn pick_usable_respects_region_and_health() {
        let mgr = ProxyManager::new(
            vec![
                entry("http://us1", Some("us"), None),
                entry("http://eu1", Some("eu"), None),
            ],
            None,
        )
        .unwrap();
        assert_eq!(mgr.pick_usable_for_region(Some("eu")), Some(2));
        assert_eq!(mgr.pick_usable_for_region(Some("us")), Some(1));
        // 没有该 region → None
        assert_eq!(mgr.pick_usable_for_region(Some("ap")), None);

        // us 代理 dead 后,挑 us 返回 None
        for _ in 0..PROXY_DEAD_THRESHOLD {
            mgr.record_health(1, false, None);
        }
        assert_eq!(mgr.pick_usable_for_region(Some("us")), None);
    }

    #[test]
    fn persist_and_reload() {
        let p = std::env::temp_dir().join(format!("xkiro-proxytest-rt-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        {
            let mgr = ProxyManager::load_from(&p).unwrap();
            mgr.add(entry("http://persisted", Some("us"), Some(3))).unwrap();
        }
        // 重新加载
        let mgr2 = ProxyManager::load_from(&p).unwrap();
        let list = mgr2.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].entry.url, "http://persisted");
        assert_eq!(list[0].entry.max_concurrency, Some(3));
        assert!(mgr2.semaphore_for(list[0].entry.id.unwrap()).is_some());
        let _ = std::fs::remove_file(&p);
    }
}
