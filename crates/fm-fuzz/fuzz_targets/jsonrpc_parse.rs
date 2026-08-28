//! fuzz: JSON-RPC UDS 行解析器。喂任意字节给 parse_line,
//! 解析成功则 re-serialize 请求 — 全程不得 panic。
//! 覆盖: 畸形 JSON / 超长 / 坏 UTF-8 / 类型混淆 / 嵌套深。
//!
//! 跑法: cargo +nightly fuzz run jsonrpc_parse

#![no_main]

use libfuzzer_sys::fuzz_target;
use fm_server::jsonrpc::parse_line;

fuzz_target!(|data: &[u8]| {
    let s = std::str::from_utf8(data)
        .unwrap_or_else(|_| String::from_utf8_lossy(data).into_owned());
    if let Some(req) = parse_line(&s) {
        let _ = serde_json::to_string(&req);
    }
});
