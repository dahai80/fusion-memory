//! #18 fusion-identity 集成: JWT verify + tid↔tenant 强制 + usage 上报。
//!
//! multi-tenant PRD §4 红线 1/2: fail-closed + cross-tenant denied。
//! fusion-identity (127.0.0.1:11470) 是生态唯一 JWT issuer + tenant registry,
//! `POST /api/v1/auth/verify` (service-token gated) 返 {tid, role, scopes, quota, tenant_status}。
//! 本模块在 fm-server HTTP 边界 trust-but-verify: 验 caller JWT → tid, 强制 tid==请求 tenant,
//! 多租户模式拒空 tenant。verify 结果短 TTL 缓存 (避免每请求一次 HTTP)。
//! `POST /api/v1/tenants/{id}/usage` 上报 storage_mb 配额指标。
//!
//! 100% offline: HTTP 只到 127.0.0.1 (PRD C8)。测试用 trait mock + 真 axum echo, 不起 fusion-identity。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// fusion-identity `/verify` 返回体 (子集 — 仅取 tid + tenant_status)。
#[derive(Debug, Clone, Deserialize)]
pub struct VerifyClaims {
    pub tid: String,
    pub tenant_status: String,
    pub revoked: bool,
}

/// #18: identity 边界校验结果。Ok(tid) 或 Err((status, body)) 供 handler 直接返。
pub type VerifyResult = Result<String, (axum::http::StatusCode, axum::Json<serde_json::Value>)>;

/// #18: identity 集成 trait — 真实 HTTP 验证器 + 测试 mock。
#[async_trait]
pub trait IdentityVerifier: Send + Sync {
    /// 验 JWT, 返权威 tid。失败返 (401, error body)。
    async fn verify(&self, jwt: &str) -> VerifyResult;
    /// 上报租户存储用量 (metric=storage_mb)。失败仅 warn, 不阻断业务 (best-effort)。
    async fn report_usage(&self, tenant: &str, metric: &str, value: u64) -> ();
}

/// #18: 真实 fusion-identity HTTP 验证器。reqwest client + 短 TTL 缓存。
pub struct RealVerifier {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
    /// jwt → (tid, expire_at) 缓存。短 TTL 省 HTTP 往返。
    cache: Arc<Mutex<HashMap<String, (String, Instant)>>>,
    cache_ttl: Duration,
}

impl RealVerifier {
    /// base_url 形如 `http://127.0.0.1:11470`。service_token gate /verify + /usage。
    pub fn new(base_url: String, service_token: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            service_token,
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_ttl: Duration::from_secs(30),
        }
    }

    fn deny(msg: &str) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({ "error": msg })),
        )
    }
}

#[async_trait]
impl IdentityVerifier for RealVerifier {
    async fn verify(&self, jwt: &str) -> VerifyResult {
        // 缓存命中?
        let cached = {
            let g = self.cache.lock().await;
            g.get(jwt).filter(|(_, exp)| *exp > Instant::now()).cloned()
        };
        if let Some((tid, _)) = cached {
            return Ok(tid);
        }
        // POST /api/v1/auth/verify, Authorization: Bearer <service_token>, body {"token": jwt}
        let url = format!("{}/api/v1/auth/verify", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.service_token))
            .json(&serde_json::json!({ "token": jwt }))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, url, "identity verify HTTP failed (fail-closed)");
                return Err(Self::deny("identity verification unavailable"));
            }
        };
        let status = resp.status();
        if !status.is_success() {
            warn!(status = status.as_u16(), "identity verify rejected token");
            return Err(Self::deny("invalid token"));
        }
        let claims: VerifyClaims = match resp.json().await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "identity verify response decode failed");
                return Err(Self::deny("invalid token"));
            }
        };
        if claims.revoked {
            warn!("identity verify: token revoked");
            return Err(Self::deny("revoked token"));
        }
        if claims.tenant_status != "active" {
            warn!(tenant_status = claims.tenant_status, "tenant not active");
            return Err(Self::deny("tenant not active"));
        }
        let tid = claims.tid.clone();
        // 写缓存
        {
            let mut g = self.cache.lock().await;
            g.insert(
                jwt.to_string(),
                (tid.clone(), Instant::now() + self.cache_ttl),
            );
        }
        info!(
            tid,
            "identity verify ok (cached {}s)",
            self.cache_ttl.as_secs()
        );
        Ok(tid)
    }

    async fn report_usage(&self, tenant: &str, metric: &str, value: u64) -> () {
        let url = format!("{}/api/v1/tenants/{}/usage", self.base_url, tenant);
        let body = serde_json::json!({
            "metric": metric,
            "value": value,
            "source": "fusion-memory",
        });
        let resp = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.service_token))
            .json(&body)
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                info!(tenant, metric, value, "identity usage reported");
            }
            Ok(r) => {
                warn!(
                    tenant,
                    metric,
                    status = r.status().as_u16(),
                    "identity usage report rejected (best-effort, not fatal)"
                );
            }
            Err(e) => {
                warn!(error = %e, tenant, metric, "identity usage report HTTP failed (best-effort)");
            }
        }
    }
}

/// #18: Noop 验证器 — multi_tenant=false (默认/向后兼容) 时用。enforce_multi_tenant 在
/// multi_tenant=false 时短路返 Ok, 故 Noop 永不被调, 仅满足 HttpState 字段类型。
pub struct NoopVerifier;

#[async_trait]
impl IdentityVerifier for NoopVerifier {
    async fn verify(&self, _jwt: &str) -> VerifyResult {
        Ok(String::new())
    }
    async fn report_usage(&self, _t: &str, _m: &str, _v: u64) -> () {}
}

/// #18: 便捷构造 NoopVerifier Arc (HttpState 默认值)。
pub fn noop_verifier() -> Arc<dyn IdentityVerifier> {
    Arc::new(NoopVerifier)
}

/// #18: 多租户模式校验 — 验 JWT + 强制 tid==tenant + 拒空 tenant。
/// multi_tenant=false 时直接返 Ok (向后兼容, 不验 identity)。
/// multi_tenant=true 时: 缺 JWT → 401; tid != tenant → 403; 空 tenant → 401 (red line 1)。
pub async fn enforce_multi_tenant(
    verifier: &dyn IdentityVerifier,
    headers: &axum::http::HeaderMap,
    tenant: &str,
    multi_tenant: bool,
) -> VerifyResult {
    if !multi_tenant {
        return Ok(tenant.to_string());
    }
    // red line 1: 多租户模式拒空 tenant (无 default-tenant 降级)。
    if tenant.is_empty() {
        warn!("multi-tenant mode: empty tenant rejected (red line 1)");
        return Err((
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": "tenant required in multi-tenant mode"
            })),
        ));
    }
    // 提取 Authorization: Bearer <jwt>。
    let jwt = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(|t| t.trim().to_string()));
    let jwt = match jwt {
        Some(j) if !j.is_empty() => j,
        _ => {
            warn!("multi-tenant mode: missing JWT bearer");
            return Err((
                axum::http::StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({
                    "error": "missing bearer JWT for identity verification"
                })),
            ));
        }
    };
    let tid = verifier.verify(&jwt).await?;
    // red line 2: tid == tenant (cross-tenant denied)。
    if tid != tenant {
        warn!(
            tid,
            tenant, "cross-tenant denied: jwt tid != request tenant"
        );
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "cross-tenant access denied: token tid does not match request tenant"
            })),
        ));
    }
    Ok(tid)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockVerifier {
        tid: String,
        ok: bool,
    }

    #[async_trait]
    impl IdentityVerifier for MockVerifier {
        async fn verify(&self, _jwt: &str) -> VerifyResult {
            if self.ok {
                Ok(self.tid.clone())
            } else {
                Err((
                    axum::http::StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": "invalid token" })),
                ))
            }
        }
        async fn report_usage(&self, _t: &str, _m: &str, _v: u64) -> () {}
    }

    fn hdr(auth: Option<&str>, tenant: Option<&str>) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        if let Some(a) = auth {
            h.insert("authorization", a.parse().unwrap());
        }
        if let Some(t) = tenant {
            h.insert("X-Fusion-Tenant", t.parse().unwrap());
        }
        h
    }

    #[tokio::test]
    async fn multi_tenant_off_no_verify() {
        let v = MockVerifier {
            tid: "acme".into(),
            ok: false,
        };
        // multi_tenant=false → 不验, 直接返 tenant (向后兼容)
        let r = enforce_multi_tenant(&v, &hdr(None, None), "", false).await;
        assert!(r.is_ok());
        assert_eq!(r.unwrap(), "");
    }

    #[tokio::test]
    async fn multi_tenant_empty_tenant_rejected() {
        let v = MockVerifier {
            tid: "acme".into(),
            ok: true,
        };
        let r = enforce_multi_tenant(&v, &hdr(Some("Bearer j"), None), "", true).await;
        assert!(r.is_err());
        let (code, _) = r.unwrap_err();
        assert_eq!(code, axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn multi_tenant_missing_jwt_rejected() {
        let v = MockVerifier {
            tid: "acme".into(),
            ok: true,
        };
        let r = enforce_multi_tenant(&v, &hdr(None, Some("acme")), "acme", true).await;
        assert!(r.is_err());
        let (code, _) = r.unwrap_err();
        assert_eq!(code, axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn multi_tenant_cross_tenant_denied() {
        let v = MockVerifier {
            tid: "acme".into(),
            ok: true,
        };
        // jwt tid=acme, request tenant=other → 403
        let r =
            enforce_multi_tenant(&v, &hdr(Some("Bearer j"), Some("other")), "other", true).await;
        assert!(r.is_err());
        let (code, _) = r.unwrap_err();
        assert_eq!(code, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn multi_tenant_matching_tid_passes() {
        let v = MockVerifier {
            tid: "acme".into(),
            ok: true,
        };
        let r = enforce_multi_tenant(&v, &hdr(Some("Bearer j"), Some("acme")), "acme", true).await;
        assert!(r.is_ok());
        assert_eq!(r.unwrap(), "acme");
    }

    #[tokio::test]
    async fn multi_tenant_invalid_jwt_rejected() {
        let v = MockVerifier {
            tid: "acme".into(),
            ok: false,
        };
        let r =
            enforce_multi_tenant(&v, &hdr(Some("Bearer bad"), Some("acme")), "acme", true).await;
        assert!(r.is_err());
        let (code, _) = r.unwrap_err();
        assert_eq!(code, axum::http::StatusCode::UNAUTHORIZED);
    }

    // #18: 真 axum echo server 当 fusion-identity, 验 RealVerifier 端到端 (100% offline)。
    #[tokio::test]
    async fn real_verifier_e2e_against_axum_echo() {
        use axum::{routing::post, Json, Router};

        let app = Router::new().route(
            "/api/v1/auth/verify",
            post(|Json(body): Json<serde_json::Value>| async move {
                let token = body.get("token").and_then(|v| v.as_str()).unwrap_or("");
                if token == "good-jwt" {
                    Json(serde_json::json!({
                        "tid": "tenant-x",
                        "tenant_status": "active",
                        "revoked": false
                    }))
                } else {
                    Json(serde_json::json!({
                        "tid": "tenant-x",
                        "tenant_status": "active",
                        "revoked": true
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // 给 listener 一点时间起
        tokio::time::sleep(Duration::from_millis(50)).await;

        let v = RealVerifier::new(format!("http://{}", addr), "svc-tok".into());
        let r = v.verify("good-jwt").await;
        assert!(r.is_ok(), "good-jwt should verify");
        assert_eq!(r.unwrap(), "tenant-x");

        // revoked → denied
        let r2 = v.verify("bad-jwt").await;
        assert!(r2.is_err());

        // 缓存命中: 第二次 good-jwt 不再打 echo (无法直接断言, 但应仍 ok)
        let r3 = v.verify("good-jwt").await;
        assert!(r3.is_ok());
    }

    // #18: RealVerifier 对不存在的 identity endpoint fail-closed。
    #[tokio::test]
    async fn real_verifier_unavailable_fail_closed() {
        let v = RealVerifier::new("http://127.0.0.1:1".into(), "svc-tok".into());
        let r = v.verify("any-jwt").await;
        assert!(r.is_err(), "unreachable identity must fail-closed");
        let (code, _) = r.unwrap_err();
        assert_eq!(code, axum::http::StatusCode::UNAUTHORIZED);
    }
}
