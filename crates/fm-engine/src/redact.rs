//! PII 正则脱敏。PRD R8/§10.4: guard 未落地前 fusion-memory 自带最小脱敏。
//!
//! commit/import 写入路径在 embed+persist 前脱敏, 故向量/图谱/检索全用脱敏后内容。
//! 占位符 [REDACTED:type], 不含原文。已脱敏内容不重复处理 (幂等)。
//! §1.16: 默认开 (fail-closed 安全默认), env FUSION_MEMORY_REDACT_PII=0/false 显式关闭 (测试/无 PII 场景)。
//! MemoryEngine::redact 字段控制; engine_builder 与 fm-py 入口读 env 决定是否调 with_redact()。

use regex::Regex;
use std::sync::OnceLock;

// PII 模式 (中文语境优先, regex crate 无 lookaround, 靠顺序敏感避免误吞):
// 0 手机号: 1[3-9] 开头 11 位, 或 +86/0086 前缀的国际写法 (§1.16 扩覆盖)
// 1 邮箱
// 2 身份证: 18 位 (末位 X), 前 17 数字
// 3 银行卡: 13-19 位连续数字 (配 Luhn 校验, 避免误吞订单号/时间戳等长数字串)
// 4 IPv4
// 5 护照: 大写字母开头 8-9 位字母数字 (中国因私护照 E+8数字 / 通用 G/E/D+8位)
// 6 IPv6: P2-2 扩覆盖。至少 2 个冒号 (含 :: 缩写), hex 段 1-4 位。避开单词/时间 (单词无冒号)。
//   覆盖 2001:db8::1 / fe80::1 / ::1 / 2001:0db8:0000:0000:0000:0000:0000:0001 全写。
// 7 国际手机: P2-2 扩覆盖。+ 后 7-15 位 (E.164), 非 86 国家码 (86 由模式 0 先脱敏)。
//   顺序在 0 之后, 故 +8613... 已被 0 替换, 此模式只命中 +1.../+44... 等。
static PATTERNS: &[&str] = &[
    r"(?:\+86|0086)?1[3-9]\d{9}",
    r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}",
    r"[1-9]\d{5}(?:19|20)\d{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])\d{3}[\dXx]",
    r"\d{13,19}",
    r"\b(?:\d{1,3}\.){3}\d{1,3}\b",
    r"\b[EeGgDd]\d{8}\b",
    // IPv6: 至少 2 冒号, 每段 1-4 hex。容忍 :: (0 次重复段)。最简实用形。
    r"\b[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{1,4}){2,7}\b|::[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{1,4}){0,6}\b",
    // 国际手机: + 后 7-15 纯数字 (E.164 最短 7 最长 15)。须紧跟非数字边界避免吞后续。
    r"\+\d{7,15}\b",
];

// 占位符标签 (与 PATTERNS 下标对应)
const TAGS: &[&str] = &[
    "phone", "email", "idcard", "bankcard", "ip", "passport",
    "ip",    // IPv6 复用 ip 标签 (P2-2)
    "phone", // 国际手机复用 phone 标签 (P2-2)
];

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
    // 顺序: 身份证(2) > 银行卡(3) > 手机(0) > 邮箱(1) > IPv4(4) > 护照(5) > IPv6(6) > 国际手机(7)
    // 国际手机(7) 须在 手机(0) 后: +8613... 先被 0 脱敏, 7 只命中非 86 国家码 (E.164)。
    let order = [2usize, 3, 0, 1, 4, 5, 6, 7];
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
/// §1.16: 默认开启 (fail-closed 安全默认)。旧版默认关 → 未显式设 env 时原始 PII 落
/// memory.db + sled + 集群明文。显式 FUSION_MEMORY_REDACT_PII=0/false 关闭 (测试/无 PII 场景)。
pub fn redact_enabled_env() -> bool {
    std::env::var("FUSION_MEMORY_REDACT_PII")
        .map(|v| parse_env_bool(&v))
        .unwrap_or(true)
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

    #[test]
    fn intl_phone_redacted() {
        // §1.16: +86/0086 前缀国际写法应脱敏
        let out = redact_text("call +8613912345678 or 008613912345678");
        assert!(out.contains("[REDACTED:phone]"), "+86 前缀应脱敏");
        assert!(!out.contains("13912345678"), "脱敏后不应残留原始号码段");
    }

    #[test]
    fn passport_redacted() {
        // §1.16: 中国因私护照 E+8 数字应脱敏
        let out = redact_text("护照号 E12345678 请登记");
        assert!(out.contains("[REDACTED:passport]"));
        assert!(!out.contains("E12345678"));
    }

    #[test]
    fn bare_phone_still_redacted_after_pattern_extend() {
        // §1.16 回归: 扩覆盖后裸 11 位手机仍命中
        let out = redact_text("电话 13912345678");
        assert!(out.contains("[REDACTED:phone]"));
    }

    #[test]
    fn ipv6_redacted() {
        // P2-2: IPv6 应脱敏 (缩写 + 全写)
        let out = redact_text("node at 2001:db8::1 and fe80::1 alive");
        assert!(out.contains("[REDACTED:ip]"), "IPv6 缩写应脱敏, got: {out}");
        assert!(!out.contains("2001:db8::1"));
        assert!(!out.contains("fe80::1"));
    }

    #[test]
    fn ipv6_full_form_redacted() {
        // P2-2: IPv6 全写 8 段
        let out = redact_text("addr 2001:0db8:0000:0000:0000:0000:0000:0001 ok");
        assert!(out.contains("[REDACTED:ip]"), "IPv6 全写应脱敏, got: {out}");
    }

    #[test]
    fn intl_phone_generic_redacted() {
        // P2-2: 非 86 国家码国际手机 (E.164) 应脱敏
        let out = redact_text("call +12125550100 or +447911123456");
        assert!(
            out.contains("[REDACTED:phone]"),
            "国际手机应脱敏, got: {out}"
        );
        assert!(!out.contains("12125550100"));
    }

    #[test]
    fn china_plus86_not_double_redacted() {
        // P2-2 回归: +8613... 由模式 0 脱敏, 不应再触发国际手机模式 (幂等, 无双重占位)
        let out = redact_text("电话 +8613912345678");
        let count = out.matches("[REDACTED:phone]").count();
        assert_eq!(count, 1, "+86 应单次脱敏, 实际 {count} 次: {out}");
    }
}
