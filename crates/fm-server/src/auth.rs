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
    if auth == expected {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "unauthorized").into_response())
    }
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
