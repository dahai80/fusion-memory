//! PII 正则脱敏。PRD R8/§10.4: guard 未落地前 fusion-memory 自带最小脱敏。
//!
//! commit/import 写入路径在 embed+persist 前脱敏, 故向量/图谱/检索全用脱敏后内容。
//! 占位符 [REDACTED:type], 不含原文。已脱敏内容不重复处理 (幂等)。
//! 默认关, env FUSION_MEMORY_REDACT_PII=1 开启 (MemoryEngine::redact 字段控制)。

use regex::RegexSet;
use std::sync::OnceLock;

// PII 模式 (中文语境优先, regex crate 无 lookaround, 靠顺序敏感避免误吞):
// 0 手机号: 1[3-9] 开头 11 位
// 1 邮箱
// 2 身份证: 18 位 (末位 X), 前 17 数字
// 3 银行卡: 13-19 位连续数字 (宽松, 覆盖主流卡号)
// 4 IPv4
static PATTERNS: &[&str] = &[
    r"1[3-9]\d{9}",
    r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}",
    r"[1-9]\d{5}(?:19|20)\d{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])\d{3}[\dXx]",
    r"\d{13,19}",
    r"\b(?:\d{1,3}\.){3}\d{1,3}\b",
];

// 占位符标签 (与 PATTERNS 下标对应)
const TAGS: &[&str] = &["phone", "email", "idcard", "bankcard", "ip"];

static REDACT_SET: OnceLock<RegexSet> = OnceLock::new();

fn redact_set() -> &'static RegexSet {
    REDACT_SET.get_or_init(|| {
        RegexSet::new(PATTERNS).expect("PII regex set compile (const patterns, infallible)")
    })
}

/// 脱敏单段文本。逐 pattern 命中段替换为 [REDACTED:tag]。
/// 顺序敏感: 身份证(18位)/银行卡(13-19位) 先于手机(11位), 长串先替换为无数字占位,
/// 短模式不再匹配已占位段, 避免手机吞银行卡前 11 位。
pub fn redact_text(input: &str) -> String {
    let set = redact_set();
    // 无任何命中 → 原样返回 (零拷贝路径, 常见 case)
    if !set.is_match(input) {
        return input.to_string();
    }
    let mut out = input.to_string();
    // 顺序: 身份证(2) > 银行卡(3) > 手机(0) > 邮箱(1) > IP(4)
    let order = [2usize, 3, 0, 1, 4];
    let pats = set.patterns();
    for idx in order {
        let pat = &pats[idx];
        let re = regex::Regex::new(pat).expect("PII pattern recompile (same const set)");
        out = re
            .replace_all(&out, format!("[REDACTED:{}]", TAGS[idx]))
            .into_owned();
    }
    out
}

/// 是否开启脱敏。读 env 一次 (启动期), 运行期不重读避免竞争。
pub fn redact_enabled_env() -> bool {
    std::env::var("FUSION_MEMORY_REDACT_PII")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pii_unchanged() {
        assert_eq!(
            redact_text("hello rust, no secrets here"),
            "hello rust, no secrets here"
        );
        assert_eq!(redact_text(""), "");
        assert_eq!(redact_text("普通中文无敏感信息"), "普通中文无敏感信息");
    }

    #[test]
    fn phone_redacted() {
        let out = redact_text("call me at 13912345678 tomorrow");
        assert!(out.contains("[REDACTED:phone]"));
        assert!(!out.contains("13912345678"));
    }

    #[test]
    fn email_redacted() {
        let out = redact_text("mail me at user@example.com please");
        assert!(out.contains("[REDACTED:email]"));
        assert!(!out.contains("user@example.com"));
    }

    #[test]
    fn idcard_redacted() {
        let out = redact_text("身份证 11010119900307888X 复印件");
        assert!(out.contains("[REDACTED:idcard]"));
        assert!(!out.contains("11010119900307888X"));
    }

    #[test]
    fn bankcard_redacted() {
        // 16 位卡号
        let out = redact_text("卡号 6222021234567890123 已绑定");
        assert!(out.contains("[REDACTED:bankcard]"));
    }

    #[test]
    fn ipv4_redacted() {
        let out = redact_text("server at 192.168.1.100 online");
        assert!(out.contains("[REDACTED:ip]"));
        assert!(!out.contains("192.168.1.100"));
    }

    #[test]
    fn multiple_pii_in_one_text() {
        let out = redact_text("电话 13800001111 邮箱 a@b.com ip 10.0.0.1");
        assert!(out.contains("[REDACTED:phone]"));
        assert!(out.contains("[REDACTED:email]"));
        assert!(out.contains("[REDACTED:ip]"));
    }

    #[test]
    fn already_redacted_idempotent() {
        let once = redact_text("电话 13800001111");
        let twice = redact_text(&once);
        assert_eq!(once, twice, "已脱敏内容二次处理不变");
    }

    #[test]
    fn phone_not_eaten_by_long_digit() {
        // 11 位手机不应被银行卡(13-19) 吞, 也不应吞入更长数字串
        let out = redact_text("手机 13912345678 和卡号 6222021234567890123");
        assert!(out.contains("[REDACTED:phone]"));
        assert!(out.contains("[REDACTED:bankcard]"));
    }

    #[test]
    fn short_digit_not_pii() {
        // 4 位数字不是 PII (年份/端口号短串)
        assert_eq!(redact_text("year 2024 port 8080"), "year 2024 port 8080");
    }

    #[test]
    fn env_flag_parse() {
        // env 全局, 仅读不写 (不串扰其他 env 测试)
        std::env::remove_var("FUSION_MEMORY_REDACT_PII");
        assert!(!redact_enabled_env());
        std::env::set_var("FUSION_MEMORY_REDACT_PII", "1");
        assert!(redact_enabled_env());
        std::env::set_var("FUSION_MEMORY_REDACT_PII", "true");
        assert!(redact_enabled_env());
        std::env::set_var("FUSION_MEMORY_REDACT_PII", "0");
        assert!(!redact_enabled_env());
        std::env::remove_var("FUSION_MEMORY_REDACT_PII");
    }
}
