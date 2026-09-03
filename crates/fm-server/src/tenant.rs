//! #16 多租户隔离: gateway 源校验 + 权威租户提取。fusion-gateway #150 Gap1c。
//!
//! gateway 在每条出站请求盖 `X-Fusion-Route: gateway-decision` (源信号) +
//! `X-Fusion-Tenant` (权威租户, 由 api_key->team 绑定派生)。本模块在 HTTP/UDS 边界
//! 提取这两个头, enforce gateway-origin (配置开时缺源 → 403), 返回请求作用域租户。
//! `X-Space-Id` 非权威透传, 永不读。tenant="" = 默认租户 (单租户向后兼容)。

use axum::http::{HeaderMap, StatusCode};
use tracing::warn;

/// gateway 源信号头值 (gateway 盖在每条出站请求)。
pub const GATEWAY_ROUTE_HEADER: &str = "X-Fusion-Route";
pub const GATEWAY_ROUTE_VALUE: &str = "gateway-decision";
/// 权威租户头。
pub const TENANT_HEADER: &str = "X-Fusion-Tenant";

/// #16: 校验 gateway 源 + 提取权威租户。
/// - `gateway_origin_required=true` 且非 public 路径: 缺/错 `X-Fusion-Route` → 403 拒绝。
/// - 租户: 有 `X-Fusion-Tenant` → 用之 (权威); 无 → 回退 default_tenant (空 = 默认租户)。
///
/// 返回 Ok(tenant) 或 Err((StatusCode, body)) 供 handler 直接返。
pub fn check_gateway_origin(
    headers: &HeaderMap,
    gateway_origin_required: bool,
    default_tenant: &str,
    is_public: bool,
    path: &str,
) -> Result<String, (StatusCode, axum::Json<serde_json::Value>)> {
    let route_origin = headers
        .get(GATEWAY_ROUTE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if gateway_origin_required && !is_public && route_origin != GATEWAY_ROUTE_VALUE {
        warn!(
            path,
            route = route_origin,
            tenant = ?headers.get(TENANT_HEADER).and_then(|v| v.to_str().ok()),
            "rejected non-gateway-origin request (#16): missing/invalid X-Fusion-Route",
        );
        return Err((
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "gateway-origin required: missing or invalid X-Fusion-Route header"
            })),
        ));
    }
    // 权威租户: X-Fusion-Tenant > default_tenant > ""。
    let tenant = headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_tenant.to_string());
    Ok(tenant)
}
