//! PII 正则脱敏。PRD R8/§10.4: guard 未落地前 fusion-memory 自带最小脱敏。
//!
//! commit/import 写入路径在 embed+persist 前脱敏, 故向量/图谱/检索全用脱敏后内容。
//! 占位符 [REDACTED:type], 不含原文。已脱敏内容不重复处理 (幂等)。
//! 默认关, env FUSION_MEMORY_REDACT_PII=1 开启 (MemoryEngine::redact 字段控制)。

use regex::Regex;
use std::sync::OnceLock;

// PII 模式 (中文语境优先, regex crate 无 lookaround, 靠顺序敏感避免误吞):
// 0 手机号: 1[3-9] 开头 11 位
// 1 邮箱
// 2 身份证: 18 位 (末位 X), 前 17 数字
// 3 银行卡: 13-19 位连续数字 (配 Luhn 校验, 避免误吞订单号/时间戳等长数字串)
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

static REDACT_REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();

fn redact_regexes() -> &'static [Regex] {
    REDACT_REGEXES.get_or_init(|| {
        PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("PII regex compile (const patterns, infallible)"))
            .collect()
    })
}

/// Luhn 校验 (银行卡号校验算法)。合法返回 true。
fn luhn_valid(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut odd = true;
    for ch in digits.chars().rev() {
        let d = match ch.to_digit(10) {
            Some(d) => d,
            None => return false,
        };
        if odd {
            sum += d;
        } else {
            let dd = d * 2;
            sum += if dd > 9 { dd - 9 } else { dd };
        }
        odd = !odd;
    }
    sum.is_multiple_of(10)
}

/// 脱敏单段文本。逐 pattern 命中段替换为 [REDACTED:tag]。
/// 顺序敏感: 身份证(18位)/银行卡(13-19位) 先于手机(11位), 长串先替换为无数字占位,
/// 短模式不再匹配已占位段, 避免手机吞银行卡前 11 位。
/// 银行卡模式额外做 Luhn 校验, 不合法的长数字串 (订单号/时间戳) 不脱敏, 避免污染内容。
pub fn redact_text(input: &str) -> String {
    let regexes = redact_regexes();
    // 零拷贝快速路径: 任一未命中 → 原样返回 (常见 case)
    if !regexes.iter().any(|re| re.is_match(input)) {
        return input.to_string();
    }
    let mut out = input.to_string();
    // 顺序: 身份证(2) > 银行卡(3) > 手机(0) > 邮箱(1) > IP(4)
    let order = [2usize, 3, 0, 1, 4];
    for idx in order {
        let re = &regexes[idx];
        if idx == 3 {
            // 银行卡: Luhn 校验, 不合法保留原文 (避误吞订单号/时间戳)
            out = re
                .replace_all(&out, |c: &regex::Captures| {
                    let digits: &str = c.get(0).map(|m| m.as_str()).unwrap_or("");
                    if luhn_valid(digits) {
                        format!("[REDACTED:{}]", TAGS[idx])
                    } else {
                        digits.to_string()
                    }
                })
                .into_owned();
        } else {
            out = re
                .replace_all(&out, format!("[REDACTED:{}]", TAGS[idx]))
                .into_owned();
        }
    }
    out
}

/// 解析 env bool 值 (纯函数, 便于单测, 避免测试里 set_var 全局竞争)。
fn parse_env_bool(v: &str) -> bool {
    v == "1" || v.eq_ignore_ascii_case("true")
}

/// 是否开启脱敏。每次调用读 env (并发读安全; 引擎层 redact 字段已缓存 builder 期结果,
/// 此 fn 仅在 engine_builder/import 路径调用, 非热路径)。
pub fn redact_enabled_env() -> bool {
    std::env::var("FUSION_MEMORY_REDACT_PII")
        .map(|v| parse_env_bool(&v))
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
        // 4242 4242 4242 4242 (Visa 测试卡, Luhn 合法)
        let out = redact_text("卡号 4242424242424242 已绑定");
        assert!(out.contains("[REDACTED:bankcard]"));
        assert!(!out.contains("4242424242424242"));
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
        // 11 位手机不被银行卡吞, Luhn 合法卡号独立脱敏
        let out = redact_text("手机 13912345678 和卡号 4242424242424242");
        assert!(out.contains("[REDACTED:phone]"));
        assert!(out.contains("[REDACTED:bankcard]"));
    }

    #[test]
    fn short_digit_not_pii() {
        // 4 位数字不是 PII (年份/端口号短串)
        assert_eq!(redact_text("year 2024 port 8080"), "year 2024 port 8080");
    }

    #[test]
    fn long_non_luhn_not_redacted() {
        // 18 位非 Luhn 数字串 (订单号/时间戳样) 不应被银行卡脱敏, 避免污染内容
        let out = redact_text("order 999999999999999999 timestamp");
        assert!(
            !out.contains("[REDACTED:bankcard]"),
            "非 Luhn 长数字不应脱敏"
        );
        assert!(out.contains("999999999999999999"));
    }

    #[test]
    fn parse_env_bool_pure() {
        // 纯函数测试, 不触碰全局 env (避 set_var 并发竞争)
        assert!(parse_env_bool("1"));
        assert!(parse_env_bool("true"));
        assert!(parse_env_bool("TRUE"));
        assert!(!parse_env_bool("0"));
        assert!(!parse_env_bool(""));
        assert!(!parse_env_bool("yes"));
    }
}
