//! 内置别名字典。PRD §7.4 A5 第 2 步。
//!
//! 归一化: 小写键 → 规范名 (保留原大小写风格)。
//! 仅覆盖常见编程/技术缩写; 运行时 LLM alias 候选写入 entity.aliases (第 3 步)。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 规范化名 (规范大写)。
const RAW: &[(&str, &str)] = &[
    ("py", "Python"),
    ("python3", "Python"),
    ("pythonlang", "Python"),
    ("rust-lang", "Rust"),
    ("rustlang", "Rust"),
    ("rs", "Rust"),
    ("ts", "TypeScript"),
    ("js", "JavaScript"),
    ("golang", "Go"),
    ("c++", "C++"),
    ("cpp", "C++"),
    ("csharp", "C#"),
    ("c#", "C#"),
    ("k8s", "Kubernetes"),
    ("kubernetes", "Kubernetes"),
    ("postgres", "PostgreSQL"),
    ("pgsql", "PostgreSQL"),
    ("sqlite3", "SQLite"),
    ("sqlite", "SQLite"),
    ("ml", "Machine Learning"),
    ("ml/mlx", "MLX"),
    ("llm", "LLM"),
    ("reactjs", "React"),
    ("vuejs", "Vue"),
    ("sveltekit", "Svelte"),
];

static DICT: OnceLock<HashMap<String, String>> = OnceLock::new();

fn dict() -> &'static HashMap<String, String> {
    DICT.get_or_init(|| {
        let mut m = HashMap::with_capacity(RAW.len());
        for (k, v) in RAW {
            m.insert(k.to_ascii_lowercase(), (*v).to_string());
        }
        m
    })
}

/// 查别名字典。命中返回规范名, 否则 None。
/// key 转小写 + 去首尾空白。
pub fn canonical(raw: &str) -> Option<String> {
    let key = raw.trim().to_ascii_lowercase();
    dict().get(&key).cloned()
}

/// 内置字典句柄 (供测试/导出)。
pub fn alias_dict() -> &'static HashMap<String, String> {
    dict()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_to_python() {
        assert_eq!(canonical("py"), Some("Python".into()));
        assert_eq!(canonical("Python3"), Some("Python".into()));
    }

    #[test]
    fn rust_variants() {
        assert_eq!(canonical("rust-lang"), Some("Rust".into()));
        assert_eq!(canonical("RustLang"), Some("Rust".into()));
        assert_eq!(canonical("rs"), Some("Rust".into()));
    }

    #[test]
    fn trim_and_case() {
        assert_eq!(canonical("  Py  "), Some("Python".into()));
        assert_eq!(canonical("K8S"), Some("Kubernetes".into()));
    }

    #[test]
    fn miss_returns_none() {
        assert_eq!(canonical("something_new"), None);
        assert_eq!(canonical(""), None);
    }

    #[test]
    fn no_false_collapse() {
        // C# 与 C++ 不应互相归一
        assert_eq!(canonical("c#"), Some("C#".into()));
        assert_eq!(canonical("cpp"), Some("C++".into()));
        assert_ne!(canonical("c#"), canonical("cpp"));
    }
}
