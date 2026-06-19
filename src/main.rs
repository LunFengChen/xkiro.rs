mod admin;
mod admin_ui;
mod anthropic;
mod common;
mod http_client;
pub mod image;
mod kiro;
mod model;
mod openai;
pub mod token;

use std::collections::HashMap;
use std::sync::Arc;

use clap::Parser;
use kiro::background_refresh::BackgroundRefreshConfig;
use kiro::endpoint::{CliEndpoint, IdeEndpoint, KiroEndpoint};
use kiro::model::credentials::{CredentialsConfig, KiroCredentials};
use kiro::provider::KiroProvider;
use kiro::token_manager::MultiTokenManager;
use model::arg::{Args, Command};
use model::config::Config;
use parking_lot::RwLock;

fn format_count_map(counts: std::collections::BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return "<empty>".to_string();
    }
    counts
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(",")
}

fn log_startup_diagnostics(
    config: &Config,
    config_path: &str,
    credentials_path: &str,
    credentials: &[KiroCredentials],
    endpoint_names: &[String],
) {
    let mut endpoint_counts = std::collections::BTreeMap::new();
    let mut group_counts = std::collections::BTreeMap::new();
    let mut source_counts = std::collections::BTreeMap::new();
    let mut api_region_counts = std::collections::BTreeMap::new();
    let mut auth_region_counts = std::collections::BTreeMap::new();
    let mut subscription_counts = std::collections::BTreeMap::new();

    let mut disabled_count = 0usize;
    let mut api_key_count = 0usize;
    let mut oauth_count = 0usize;
    let mut with_proxy_id_count = 0usize;
    let mut with_proxy_url_count = 0usize;
    let mut supports_opus_count = 0usize;
    let mut explicit_endpoint_count = 0usize;

    for cred in credentials {
        if cred.disabled {
            disabled_count += 1;
        }
        if cred.is_api_key_credential() {
            api_key_count += 1;
        } else {
            oauth_count += 1;
        }
        if cred.proxy_id.is_some() {
            with_proxy_id_count += 1;
        }
        if cred.proxy_url.is_some() {
            with_proxy_url_count += 1;
        }
        if cred.supports_opus() {
            supports_opus_count += 1;
        }
        if cred.endpoint.is_some() {
            explicit_endpoint_count += 1;
        }

        *endpoint_counts
            .entry(
                cred.endpoint
                    .as_deref()
                    .unwrap_or(&config.default_endpoint)
                    .to_string(),
            )
            .or_insert(0) += 1;
        *group_counts
            .entry(cred.group.as_deref().unwrap_or("<none>").to_string())
            .or_insert(0) += 1;
        *source_counts
            .entry(cred.source.as_deref().unwrap_or("<none>").to_string())
            .or_insert(0) += 1;
        *api_region_counts
            .entry(cred.effective_api_region(config).to_string())
            .or_insert(0) += 1;
        *auth_region_counts
            .entry(cred.effective_auth_region(config).to_string())
            .or_insert(0) += 1;

        let subscription_key = match cred.subscription_title.as_deref() {
            Some(title) if title.to_uppercase().contains("FREE") => "free",
            Some(_) => "paid_or_named",
            None => "<unknown>",
        };
        *subscription_counts
            .entry(subscription_key.to_string())
            .or_insert(0) += 1;
    }

    let registered_endpoints = {
        let mut names = endpoint_names.to_vec();
        names.sort();
        names.join(",")
    };
    let global_proxy_url = config
        .proxy_url
        .as_deref()
        .map(crate::common::redact::mask_url_userinfo)
        .unwrap_or_else(|| "<none>".to_string());

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        git_sha = option_env!("VERGEN_GIT_SHA").unwrap_or("unknown"),
        config_path,
        credentials_path,
        host = %config.host,
        port = config.port,
        default_endpoint = %config.default_endpoint,
        registered_endpoints = %registered_endpoints,
        tls_backend = ?config.tls_backend,
        kiro_version = %config.kiro_version,
        system_version = %config.system_version,
        node_version = %config.node_version,
        global_proxy_url = %global_proxy_url,
        per_credential_concurrency = config.per_credential_concurrency,
        global_concurrency = config.global_concurrency,
        acquire_wait_timeout_secs = config.acquire_wait_timeout_secs,
        session_affinity_enabled = config.session_affinity_enabled,
        balance_refresh_enabled = config.balance_refresh_enabled,
        balance_refresh_interval_secs = config.balance_refresh_interval_secs,
        balance_refresh_concurrency = config.balance_refresh_concurrency,
        prompt_cache_accounting_enabled = config.prompt_cache_accounting_enabled,
        prompt_cache_ttl_seconds = config.prompt_cache_ttl_seconds,
        compression_enabled = config.compression.enabled,
        max_request_body_bytes = config.compression.max_request_body_bytes,
        precise_token_counting = config.precise_token_counting,
        "xkiro 启动诊断配置"
    );

    tracing::info!(
        total_credentials = credentials.len(),
        disabled_credentials = disabled_count,
        enabled_credentials = credentials.len().saturating_sub(disabled_count),
        api_key_credentials = api_key_count,
        oauth_credentials = oauth_count,
        supports_opus_credentials = supports_opus_count,
        with_proxy_id = with_proxy_id_count,
        with_proxy_url = with_proxy_url_count,
        explicit_endpoint_credentials = explicit_endpoint_count,
        endpoint_counts = %format_count_map(endpoint_counts),
        group_counts = %format_count_map(group_counts),
        source_counts = %format_count_map(source_counts),
        api_region_counts = %format_count_map(api_region_counts),
        auth_region_counts = %format_count_map(auth_region_counts),
        subscription_counts = %format_count_map(subscription_counts),
        "xkiro 启动凭据摘要"
    );
}

#[tokio::main]
async fn main() {
    // 解析命令行参数
    let args = Args::parse();

    // 子命令优先：init 走交互式向导直接退出，不进入服务启动流程
    if let Some(Command::Init { force }) = args.command {
        let config_path = args
            .config
            .unwrap_or_else(|| Config::default_config_path().to_string());
        if let Err(e) = model::init::run_init(std::path::Path::new(&config_path), force) {
            eprintln!("初始化失败: {:#}", e);
            std::process::exit(1);
        }
        return;
    }

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        git_sha = option_env!("VERGEN_GIT_SHA").unwrap_or("unknown"),
        "xkiro-rs 进程启动"
    );

    // 加载配置：文件不存在则进交互式向导（运行 init 流程后再加载）
    let config_path = args
        .config
        .unwrap_or_else(|| Config::default_config_path().to_string());
    if !std::path::Path::new(&config_path).exists() {
        eprintln!("未检测到配置文件 {}，进入初始化向导。", config_path);
        if let Err(e) = model::init::run_init(std::path::Path::new(&config_path), false) {
            eprintln!("初始化失败: {:#}", e);
            std::process::exit(1);
        }
    }
    let config = Config::load(&config_path).unwrap_or_else(|e| {
        tracing::error!("加载配置失败: {}", e);
        std::process::exit(1);
    });

    // 加载凭证（支持单对象或数组格式）
    let credentials_path = args
        .credentials
        .unwrap_or_else(|| KiroCredentials::default_credentials_path().to_string());
    let credentials_config = CredentialsConfig::load(&credentials_path).unwrap_or_else(|e| {
        tracing::error!("加载凭证失败: {}", e);
        std::process::exit(1);
    });

    // 判断是否为多凭据格式（用于刷新后回写）
    let is_multiple_format = credentials_config.is_multiple();

    // 转换为按优先级排序的凭据列表
    let mut credentials_list = credentials_config.into_sorted_credentials();

    // 检查 KIRO_API_KEY 环境变量，自动创建 API Key 凭据
    if let Ok(kiro_api_key) = std::env::var("KIRO_API_KEY") {
        if kiro_api_key.is_empty() {
            tracing::warn!("KIRO_API_KEY 环境变量已设置但为空，视为未配置");
        } else {
            tracing::info!("检测到 KIRO_API_KEY 环境变量，添加 API Key 凭据（最高优先级）");
            let api_key_cred = KiroCredentials {
                kiro_api_key: Some(kiro_api_key),
                auth_method: Some("api_key".to_string()),
                priority: 0,
                ..Default::default()
            };
            credentials_list.insert(0, api_key_cred);
        }
    }

    tracing::info!("已加载 {} 个凭据配置", credentials_list.len());

    // 获取第一个凭据用于日志显示
    let first_credentials = credentials_list.first().cloned().unwrap_or_default();
    tracing::debug!("主凭证: {:?}", first_credentials);

    // 获取 API Key
    let api_key = config.api_key.clone().unwrap_or_else(|| {
        tracing::error!("配置文件中未设置 apiKey");
        std::process::exit(1);
    });

    // 构建代理配置
    let proxy_config = config.proxy_url.as_ref().map(|url| {
        let mut proxy = http_client::ProxyConfig::new(url);
        if let (Some(username), Some(password)) = (&config.proxy_username, &config.proxy_password) {
            proxy = proxy.with_auth(username, password);
        }
        proxy
    });

    if proxy_config.is_some() {
        tracing::info!("已配置 HTTP 代理: {}", config.proxy_url.as_ref().unwrap());
    }

    // 构建端点注册表
    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    {
        let ide = IdeEndpoint::new();
        endpoints.insert(ide.name().to_string(), Arc::new(ide));
        let cli = CliEndpoint::new();
        endpoints.insert(cli.name().to_string(), Arc::new(cli));
    }

    // 校验默认端点存在
    if !endpoints.contains_key(&config.default_endpoint) {
        tracing::error!("默认端点 \"{}\" 未注册", config.default_endpoint);
        std::process::exit(1);
    }

    // 校验所有凭据声明的端点都已注册
    for cred in &credentials_list {
        let name = cred
            .endpoint
            .as_deref()
            .unwrap_or(&config.default_endpoint);
        if !endpoints.contains_key(name) {
            tracing::error!(
                "凭据 id={:?} 指定了未知端点 \"{}\"（已注册: {:?}）",
                cred.id,
                name,
                endpoints.keys().collect::<Vec<_>>()
            );
            std::process::exit(1);
        }
    }

    let endpoint_names: Vec<String> = endpoints.keys().cloned().collect();
    log_startup_diagnostics(
        &config,
        &config_path,
        &credentials_path,
        &credentials_list,
        &endpoint_names,
    );

    // 创建 MultiTokenManager 和 KiroProvider
    let token_manager = MultiTokenManager::new(
        config.clone(),
        credentials_list,
        proxy_config.clone(),
        Some(credentials_path.clone().into()),
        is_multiple_format,
    )
    .unwrap_or_else(|e| {
        tracing::error!("创建 Token 管理器失败: {}", e);
        std::process::exit(1);
    });
    let token_manager = Arc::new(token_manager);

    // 代理池:与 credentials.json 同目录的 proxies.json。引用式绑定的运行时来源。
    let proxies_path = std::path::Path::new(&credentials_path)
        .parent()
        .map(|dir| dir.join(kiro::proxy_manager::ProxyEntry::default_proxies_path()))
        .unwrap_or_else(|| {
            std::path::PathBuf::from(kiro::proxy_manager::ProxyEntry::default_proxies_path())
        });
    let proxy_manager = Arc::new(
        kiro::proxy_manager::ProxyManager::load_from(&proxies_path).unwrap_or_else(|e| {
            tracing::error!("加载代理池失败: {}", e);
            std::process::exit(1);
        }),
    );
    token_manager.set_proxy_manager(Arc::clone(&proxy_manager));
    let proxy_views = proxy_manager.list();
    let proxy_total = proxy_views.len();
    let proxy_disabled = proxy_views.iter().filter(|p| p.entry.disabled).count();
    let proxy_dead = proxy_views.iter().filter(|p| p.health.dead).count();
    let proxy_with_auth = proxy_views
        .iter()
        .filter(|p| p.entry.username.is_some() || p.entry.password.is_some())
        .count();
    let proxy_with_limit = proxy_views
        .iter()
        .filter(|p| p.entry.max_concurrency.unwrap_or(0) > 0)
        .count();
    let mut proxy_region_counts = std::collections::BTreeMap::new();
    for p in &proxy_views {
        *proxy_region_counts
            .entry(p.entry.region.as_deref().unwrap_or("<none>").to_string())
            .or_insert(0) += 1;
    }
    tracing::info!(
        proxy_total,
        proxy_enabled = proxy_total.saturating_sub(proxy_disabled),
        proxy_disabled,
        proxy_dead,
        proxy_with_auth,
        proxy_with_limit,
        proxy_region_counts = %format_count_map(proxy_region_counts),
        proxies_path = %proxies_path.display(),
        "代理池已加载"
    );

    // 启动后台 Token 刷新任务（默认配置：每 60s 检查一次，提前 15 分钟刷新）
    let _background_refresher =
        token_manager.start_background_refresh(BackgroundRefreshConfig::default());
    tracing::info!("后台 Token 刷新任务已启动");

    // 余额初始化由 AdminService::prefetch_balances_on_startup 统一负责：
    // 一次上游 getUsageLimits → 同时回填磁盘缓存（dashboard）+ 运行时缓存（路由决策）+ 低余额禁用

    let kiro_provider = KiroProvider::with_proxy(
        token_manager.clone(),
        proxy_config.clone(),
        endpoints,
        config.default_endpoint.clone(),
    );
    let kiro_provider = Arc::new(kiro_provider);

    // 初始化 count_tokens 配置
    token::init_config(token::CountTokensConfig {
        api_url: config.count_tokens_api_url.clone(),
        api_key: config.count_tokens_api_key.clone(),
        auth_type: config.count_tokens_auth_type.clone(),
        proxy: proxy_config,
        tls_backend: config.tls_backend,
    });

    // tiktoken cl100k_base 精确计数开关（admin API 不暴露热改，需重启）
    token::set_precise_counting(config.precise_token_counting);

    // 共享压缩配置（admin API 可运行时修改）
    let compression_config = Arc::new(RwLock::new(config.compression.clone()));

    // 共享系统提示清洗配置（admin API 可运行时修改）
    let prompt_filter_config = Arc::new(RwLock::new(config.prompt_filter.clone()));

    // 共享系统提示注入运行时配置（admin API 可运行时修改）
    let prompt_runtime = crate::model::runtime::shared_from_config(&config);

    // Prompt Cache 运行时（共享引用，支持热更新）
    // Prompt Cache 运行时（共享引用，支持热更新）
    let prompt_cache_runtime = Arc::new(RwLock::new(
        anthropic::middleware::PromptCacheRuntime::new(
            config.prompt_cache_ttl_seconds,
            config.prompt_cache_accounting_enabled,
        ),
    ));

    // 截断恢复识别开关（admin API 可运行时修改）
    let truncation_recovery_notice = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
        config.truncation_recovery_system_notice,
    ));

    // 共享多 API Key 列表（admin API 可运行时增删，鉴权即时生效）
    let shared_api_keys: anthropic::middleware::SharedApiKeys =
        Arc::new(RwLock::new(config.api_keys.clone()));

    // 请求时序埋点（Dashboard 数据源，2000 条环形缓冲）
    let shared_metrics: crate::admin::metrics::SharedMetrics =
        Arc::new(crate::admin::metrics::MetricsStore::new(2000));

    // 构建 Anthropic API 路由（profile_arn 由首个凭据提供）
    let anthropic_app = anthropic::create_router_with_provider(
        &api_key,
        shared_api_keys.clone(),
        Some(kiro_provider.clone()),
        first_credentials.profile_arn.clone(),
        config.extract_thinking,
        compression_config.clone(),
        prompt_filter_config.clone(),
        prompt_runtime.clone(),
        prompt_cache_runtime.clone(),
        truncation_recovery_notice.clone(),
        Some(shared_metrics.clone()),
    );

    // 构建 Admin API 路由（如果配置了非空的 admin_api_key）
    // 安全检查：空字符串被视为未配置，防止空 key 绕过认证
    let admin_key_valid = config
        .admin_api_key
        .as_ref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);

    let app = if let Some(admin_key) = &config.admin_api_key {
        if admin_key.trim().is_empty() {
            tracing::warn!("admin_api_key 配置为空，Admin API 未启用");
            anthropic_app
        } else {
            let admin_service = admin::AdminService::new(
                token_manager.clone(),
                Some(kiro_provider.clone()),
                compression_config.clone(),
                prompt_cache_runtime.clone(),
                prompt_runtime.clone(),
                truncation_recovery_notice.clone(),
                endpoint_names.clone(),
                proxy_manager.clone(),
                shared_api_keys.clone(),
                Some(std::path::PathBuf::from(&config_path)),
                shared_metrics.clone(),
            );
            let admin_state = admin::AdminState::new(admin_key, admin_service, compression_config.clone());

            // 注册 credit usage 观察者：metering 事件透传时同步更新 admin disk cache
            {
                let observer: std::sync::Arc<dyn crate::kiro::token_manager::CreditUsageObserver> =
                    admin_state.service.clone();
                token_manager.set_credit_observer(std::sync::Arc::downgrade(&observer));
            }

            // 启动时后台并行预取所有未禁用凭据余额，写入 disk-cache
            // 让前端首次访问 dashboard 时就能从 /balances/cached 直接拿到完整快照
            {
                let svc = admin_state.service.clone();
                tokio::spawn(async move { svc.prefetch_balances_on_startup().await });
            }
            // 启动周期性余额刷新：周期/并发/启停由 config.balance_refresh_* 控制（热更新）
            // 同步两层缓存（admin disk + token_manager 运行时），低余额自动禁用
            admin_state
                .service
                .clone()
                .start_periodic_balance_refresh();

            // 启动代理池健康巡检（每 60s 探测；死代理上的号按 region 自动换绑）
            admin_state
                .service
                .clone()
                .start_proxy_health_patrol(60);

            let admin_app = admin::create_admin_router(admin_state);

            // 创建 Admin UI 路由
            let admin_ui_app = admin_ui::create_admin_ui_router();

            tracing::info!("Admin API 已启用");
            tracing::info!("Admin UI 已启用: /admin");
            anthropic_app
                .nest("/api/admin", admin_app)
                .nest("/admin", admin_ui_app)
        }
    } else {
        anthropic_app
    };

    // 启动服务器
    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("启动 Anthropic API 端点: {}", addr);
    tracing::info!("API Key: {}***", &api_key[..(api_key.len() / 2)]);
    tracing::info!("可用 API:");
    tracing::info!("  GET  /v1/models");
    tracing::info!("  POST /v1/messages");
    tracing::info!("  POST /v1/messages/count_tokens");
    tracing::info!("  POST /v1/chat/completions");
    tracing::info!("  POST /v1/responses");
    if admin_key_valid {
        tracing::info!("Admin API:");
        tracing::info!("  GET  /api/admin/credentials");
        tracing::info!("  POST /api/admin/credentials/:index/disabled");
        tracing::info!("  POST /api/admin/credentials/:index/priority");
        tracing::info!("  POST /api/admin/credentials/:index/reset");
        tracing::info!("  GET  /api/admin/credentials/:index/balance");
        tracing::info!("Admin UI:");
        tracing::info!("  GET  /admin");
    }

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
