//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post},
};

use super::{
    handlers::{
        add_credential, delete_credential, force_refresh_token, get_all_credentials,
        get_compression_config, get_credential_balance, get_global_config, get_load_balancing_mode,
        get_proxy_config, reset_failure_count, set_compression_config, set_credential_disabled,
        set_credential_endpoint, set_credential_priority, set_credential_region,
        set_load_balancing_mode, update_global_config, update_proxy_config,
    },
    middleware::{AdminState, admin_auth_middleware},
};

/// 创建 Admin API 路由
///
/// # 端点
/// - `GET /credentials` - 获取所有凭据状态
/// - `POST /credentials` - 添加新凭据
/// - `DELETE /credentials/:id` - 删除凭据
/// - `POST /credentials/:id/disabled` - 设置凭据禁用状态
/// - `POST /credentials/:id/priority` - 设置凭据优先级
/// - `POST /credentials/:id/reset` - 重置失败计数
/// - `POST /credentials/:id/refresh` - 强制刷新 Token
/// - `GET /credentials/:id/balance` - 获取凭据余额
/// - `POST /credentials/:id/region` - 设置凭据 Region
/// - `POST /credentials/:id/endpoint` - 设置凭据 endpoint
/// - `GET /config/load-balancing` - 获取负载均衡模式
/// - `PUT /config/load-balancing` - 设置负载均衡模式
/// - `GET /config/compression` - 获取压缩配置
/// - `PUT /config/compression` - 更新压缩配置
/// - `GET /config/global` - 获取全局配置
/// - `PUT /config/global` - 更新全局配置（热更新）
/// - `GET /proxy` - 获取全局代理配置
/// - `POST /proxy` - 更新全局代理配置（热更新）
///
/// # 认证
/// 需要 Admin API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/credentials",
            get(get_all_credentials).post(add_credential),
        )
        .route("/credentials/{id}", delete(delete_credential))
        .route("/credentials/{id}/disabled", post(set_credential_disabled))
        .route("/credentials/{id}/priority", post(set_credential_priority))
        .route("/credentials/{id}/reset", post(reset_failure_count))
        .route("/credentials/{id}/refresh", post(force_refresh_token))
        .route("/credentials/{id}/balance", get(get_credential_balance))
        .route("/credentials/{id}/region", post(set_credential_region))
        .route("/credentials/{id}/endpoint", post(set_credential_endpoint))
        .route(
            "/config/load-balancing",
            get(get_load_balancing_mode).put(set_load_balancing_mode),
        )
        .route(
            "/config/compression",
            get(get_compression_config).put(set_compression_config),
        )
        .route(
            "/config/global",
            get(get_global_config).put(update_global_config),
        )
        .route("/proxy", get(get_proxy_config).post(update_proxy_config))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .with_state(state)
}
