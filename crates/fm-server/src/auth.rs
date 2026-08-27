//! HTTP Bearer 鉴权。PRD §11.2 B5 修正。
//!
//! Authorization: Bearer <token>，token == ServerConfig.api_key。不匹配 → 401。

// axum::Response 体积大（含 Body），result_large_err 在此为已知，允许。
#![allow(clippy::result_large_err)]

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

/// 校验 Bearer。返回 Ok(()) 或 401 响应。
pub fn check_bearer(headers: &HeaderMap, api_key: &str) -> Result<(), Response> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {api_key}");
    // §2.12: 旧版 `auth == expected` 用 String::eq 首字节失配即短路 → 时序侧信道逐字节泄漏 token。
    // 改: 常时比较 (同 fm-cluster transport.rs), 全字节遍历 XOR 累积, 不短路。
    if constant_time_eq(auth.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "unauthorized").into_response())
    }
}

/// §2.12: 常时字节比较。长度不等先返 false (长度本身非敏感), 等长则全字节 XOR 累积不短路。
/// 与 fm-cluster/src/transport.rs::constant_time_eq 同模式, 避免引入外部 crate。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn valid_bearer_passes() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer sekret"));
        assert!(check_bearer(&h, "sekret").is_ok());
    }

    #[test]
    fn missing_header_rejected() {
        let h = HeaderMap::new();
        assert!(check_bearer(&h, "sekret").is_err());
    }

    #[test]
    fn wrong_token_rejected() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer wrong"));
        assert!(check_bearer(&h, "sekret").is_err());
    }

    #[test]
    fn non_bearer_scheme_rejected() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Basic sekret"));
        assert!(check_bearer(&h, "sekret").is_err());
    }
}
