//! v1.0.0 B-1: 静态加密。AES-256-GCM 敏感字段加密 (defense-in-depth)。
//!
//! 分层策略:
//! - FDE (FileVault/LUKS) = 主静态加密, ops 层, 见 deploy/README.md
//! - app 层 (本模块) = 纵深防御: SQLite content/entities_json 列加密,
//!   即使磁盘快照泄露也非明文
//! - 向量不 app 加密 (hnsw_rs 需明文算距离), FDE + 上游 PII 脱敏覆盖
//!
//! key 来源 (二选一, 优先 file):
//! - FUSION_MEMORY_ENC_KEY_FILE: 0600 文件, 32B 原始 key
//! - FUSION_MEMORY_ENC_PASSPHRASE: argon2 KDF 派生 32B key
//!
//! 无 key = 明文模式 (兼容旧行为), 加密值带 "enc:v1:" 前缀供读取端识别。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use tracing::warn;

use crate::error::PersistError;
use crate::PersistResult;

const ENC_PREFIX: &str = "enc:v1:";
const KEY_LEN: usize = 32;

/// AES-256-GCM cipher + key。None = 明文模式。
pub struct Cipher {
    key: [u8; KEY_LEN],
}

impl Cipher {
    /// 从 env 构建 cipher。无 env 配置 → 返回 None (明文模式)。
    /// 优先 FUSION_MEMORY_ENC_KEY_FILE (0600, 32B 原始 key), 次 FUSION_MEMORY_ENC_PASSPHRASE (argon2 派生)。
    pub fn from_env() -> PersistResult<Option<Self>> {
        if let Ok(path) = std::env::var("FUSION_MEMORY_ENC_KEY_FILE") {
            let raw = std::fs::read(&path)
                .map_err(|e| PersistError::Encrypt(format!("read key file {path}: {e}")))?;
            if raw.len() != KEY_LEN {
                return Err(PersistError::Encrypt(format!(
                    "key file {path} must be {KEY_LEN} bytes, got {}",
                    raw.len()
                )));
            }
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&raw);
            tracing::info!(file = %path, "encryption enabled (key file)");
            return Ok(Some(Self { key }));
        }
        if let Ok(pass) = std::env::var("FUSION_MEMORY_ENC_PASSPHRASE") {
            // argon2id 派生 32B key。固定 salt (非随机) 使同口令派生同 key (可重启复用)。
            // 注: salt 固定不抗彩虹表, 但口令本就高熵 + 本机离线无远程攻击面。
            let salt = b"fusion-memory-static-encryption-v1";
            let params = argon2::Params::new(64 * 1024, 3, 4, Some(KEY_LEN))
                .map_err(|e| PersistError::Encrypt(format!("argon2 params: {e}")))?;
            let argon =
                argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
            let mut key = [0u8; KEY_LEN];
            argon
                .hash_password_into(pass.as_bytes(), salt, &mut key)
                .map_err(|e| PersistError::Encrypt(format!("argon2 derive: {e}")))?;
            tracing::info!("encryption enabled (passphrase, argon2id)");
            return Ok(Some(Self { key }));
        }
        Ok(None)
    }

    /// 从原始 32B key 构建。
    pub fn from_raw(key: [u8; KEY_LEN]) -> Self {
        Self { key }
    }

    /// 加密明文 → "enc:v1:" + base64(nonce‖ct‖tag)。
    pub fn encrypt(&self, plain: &str) -> PersistResult<String> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| PersistError::Encrypt(format!("aes init: {e}")))?;
        let mut nonce_bytes = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(nonce, plain.as_bytes())
            .map_err(|e| PersistError::Encrypt(format!("aes encrypt: {e}")))?;
        // nonce ‖ ct (ct 含 GCM tag, aes-gcm 0.10 默认附加 16B tag)
        let mut blob = Vec::with_capacity(nonce_bytes.len() + ct.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ct);
        Ok(format!("{ENC_PREFIX}{}", B64.encode(&blob)))
    }

    /// 解密。值无 enc 前缀 → 原样返回 (兼容旧行/明文模式)。
    /// 前缀在但解密失败 → warn 留痕, 返回原值 (fail-open 保服务连续, 非 panic)。
    pub fn decrypt(&self, stored: &str) -> String {
        if !stored.starts_with(ENC_PREFIX) {
            return stored.to_string();
        }
        let blob = match B64.decode(&stored[ENC_PREFIX.len()..]) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "decrypt: base64 decode failed, returning raw");
                return stored.to_string();
            }
        };
        if blob.len() < 12 {
            warn!(len = blob.len(), "decrypt: blob too short, returning raw");
            return stored.to_string();
        }
        let (nonce_bytes, ct) = blob.split_at(12);
        let cipher = match Aes256Gcm::new_from_slice(&self.key) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "decrypt: aes init failed, returning raw");
                return stored.to_string();
            }
        };
        match cipher.decrypt(Nonce::from_slice(nonce_bytes), ct) {
            Ok(pt) => String::from_utf8_lossy(&pt).into_owned(),
            Err(e) => {
                warn!(error = %e, "decrypt: aes decrypt failed (wrong key?), returning raw");
                stored.to_string()
            }
        }
    }
}

/// 顶层辅助: 用 Option<Cipher> 解密 (None = 原样返回)。
pub fn decrypt_field(cipher: Option<&Cipher>, stored: &str) -> String {
    match cipher {
        Some(c) => c.decrypt(stored),
        None => stored.to_string(),
    }
}

/// 顶层辅助: 用 Option<Cipher> 加密 (None = 原样返回)。
pub fn encrypt_field(cipher: Option<&Cipher>, plain: &str) -> PersistResult<String> {
    match cipher {
        Some(c) => c.encrypt(plain),
        None => Ok(plain.to_string()),
    }
}
