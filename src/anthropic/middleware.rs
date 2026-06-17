//! Anthropic API 中间件

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use parking_lot::RwLock;

use crate::common::auth;
use crate::kiro::provider::KiroProvider;
use crate::model::config::{ApiKeyEntry, CompressionConfig, PromptFilterConfig};
use crate::model::runtime::{PromptRuntimeConfig, SharedPromptConfig};

/// Request extension：鉴权时解析出的账号组（来自 api_keys 配置的 group 字段）
/// None = 无分组限制（单 api_key 兼容模式，或 api_keys 中未配 group 的条目）
#[derive(Clone, Debug)]
pub struct AuthGroup(pub Option<String>);

use super::cache_tracker::CacheTracker;
use super::types::ErrorResponse;

#[derive(Clone)]
pub(crate) struct PromptCacheSnapshot {
    pub accounting_enabled: bool,
    #[allow(dead_code)]
    pub ttl_seconds: u64,
    pub tracker: Arc<CacheTracker>,
}

pub struct PromptCacheRuntime {
    accounting_enabled: bool,
    ttl_seconds: u64,
    tracker: Arc<CacheTracker>,
}

impl PromptCacheRuntime {
    pub fn new(ttl_seconds: u64, accounting_enabled: bool) -> Self {
        Self {
            accounting_enabled,
            ttl_seconds,
            tracker: Arc::new(CacheTracker::new(Duration::from_secs(ttl_seconds))),
        }
    }

    pub fn snapshot(&self) -> PromptCacheSnapshot {
        PromptCacheSnapshot {
            accounting_enabled: self.accounting_enabled,
            ttl_seconds: self.ttl_seconds,
            tracker: self.tracker.clone(),
        }
    }

    pub fn update(&mut self, ttl_seconds: Option<u64>, accounting_enabled: Option<bool>) {
        if let Some(value) = accounting_enabled {
            self.accounting_enabled = value;
        }

        if let Some(value) = ttl_seconds
            && self.ttl_seconds != value
        {
            self.ttl_seconds = value;
            self.tracker = Arc::new(CacheTracker::new(Duration::from_secs(value)));
        }
    }
}

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    /// API 密钥（单 key 兼容模式）
    pub api_key: String,
    /// 多 API Key 配置（带可选分组）
    /// 非空时鉴权优先查此列表，空时回退到 api_key 单 key 模式
    pub api_keys: Vec<ApiKeyEntry>,
    /// Kiro Provider（可选，用于实际 API 调用）
    /// 内部使用 MultiTokenManager，已支持线程安全的多凭据管理
    pub kiro_provider: Option<Arc<KiroProvider>>,
    /// 是否开启非流式响应的 thinking 块提取
    pub extract_thinking: bool,
    /// Profile ARN（可选，用于请求注入）
    pub profile_arn: Option<String>,
    /// 共享压缩配置（运行时可修改）
    pub compression_config: Arc<RwLock<CompressionConfig>>,
    /// 共享系统提示清洗配置（运行时可修改）
    pub prompt_filter_config: Arc<RwLock<PromptFilterConfig>>,
    /// 共享系统提示注入运行时配置（运行时可修改）
    pub prompt_runtime: SharedPromptConfig,
    /// Prompt Cache 运行时配置（共享引用，支持热更新）
    pub prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
    /// 是否在 system prompt 末尾注入截断恢复识别说明（运行时可改）
    pub truncation_recovery_notice: Arc<AtomicBool>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(
        api_key: impl Into<String>,
        extract_thinking: bool,
        prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
        truncation_recovery_notice: Arc<AtomicBool>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            api_keys: vec![],
            kiro_provider: None,
            extract_thinking,
            profile_arn: None,
            compression_config: Arc::new(RwLock::new(CompressionConfig::default())),
            prompt_filter_config: Arc::new(RwLock::new(PromptFilterConfig::default())),
            prompt_runtime: Arc::new(RwLock::new(PromptRuntimeConfig {
                enabled: false,
                enabled_presets: Vec::new(),
                user_presets: Vec::new(),
                custom_content: None,
                position: crate::model::config::SystemPromptPosition::default(),
            })),
            prompt_cache_runtime,
            truncation_recovery_notice,
        }
    }

    /// 设置 KiroProvider
    pub fn with_kiro_provider(mut self, provider: Arc<KiroProvider>) -> Self {
        self.kiro_provider = Some(provider);
        self
    }

    /// 设置 Profile ARN
    #[allow(dead_code)]
    pub fn with_profile_arn(mut self, arn: impl Into<String>) -> Self {
        self.profile_arn = Some(arn.into());
        self
    }

    /// 设置压缩配置（接受共享引用）
    pub fn with_compression_config(mut self, config: Arc<RwLock<CompressionConfig>>) -> Self {
        self.compression_config = config;
        self
    }

    /// 设置系统提示清洗配置（接受共享引用）
    pub fn with_prompt_filter_config(mut self, config: Arc<RwLock<PromptFilterConfig>>) -> Self {
        self.prompt_filter_config = config;
        self
    }

    /// 设置系统提示注入运行时配置（接受共享引用）
    pub fn with_prompt_runtime(mut self, runtime: SharedPromptConfig) -> Self {
        self.prompt_runtime = runtime;
        self
    }

    pub fn prompt_cache_snapshot(&self) -> PromptCacheSnapshot {
        self.prompt_cache_runtime.read().snapshot()
    }

    pub fn truncation_recovery_notice_enabled(&self) -> bool {
        self.truncation_recovery_notice.load(Ordering::Relaxed)
    }
}

/// API Key 认证中间件
///
/// 鉴权逻辑：
/// 1. 若 `api_keys` 非空，按列表顺序做 constant-time 比较，匹配则注入 `AuthGroup`
/// 2. 否则回退到 `api_key` 单 key 模式（AuthGroup = None）
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(key) = auth::extract_api_key(&request) else {
        let error = ErrorResponse::authentication_error();
        return (StatusCode::UNAUTHORIZED, Json(error)).into_response();
    };

    if !state.api_keys.is_empty() {
        // 多 key 模式：查匹配，写入解析出的分组
        if let Some(entry) = state
            .api_keys
            .iter()
            .find(|e| auth::constant_time_eq(&key, &e.key))
        {
            request.extensions_mut().insert(AuthGroup(entry.group.clone()));
            return next.run(request).await;
        }
        let error = ErrorResponse::authentication_error();
        return (StatusCode::UNAUTHORIZED, Json(error)).into_response();
    }

    // 单 key 兼容模式
    if auth::constant_time_eq(&key, &state.api_key) {
        request.extensions_mut().insert(AuthGroup(None));
        next.run(request).await
    } else {
        let error = ErrorResponse::authentication_error();
        (StatusCode::UNAUTHORIZED, Json(error)).into_response()
    }
}

/// CORS 中间件层
///
/// **安全说明**：当前配置允许所有来源（Any），这是为了支持公开 API 服务。
/// 如果需要更严格的安全控制，请根据实际需求配置具体的允许来源、方法和头信息。
///
/// # 配置说明
/// - `allow_origin(Any)`: 允许任何来源的请求
/// - `allow_methods(Any)`: 允许任何 HTTP 方法
/// - `allow_headers(Any)`: 允许任何请求头
pub fn cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{Any, CorsLayer};

    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
}
