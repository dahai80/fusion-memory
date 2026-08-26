//! 数据目录解析。默认 ~/.fusion-memory。

use std::path::PathBuf;

pub fn resolve_home(home: &Option<String>) -> PathBuf {
    if let Some(h) = home {
        return PathBuf::from(h);
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".fusion-memory");
    }
    PathBuf::from(".fusion-memory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_home_wins() {
        let p = resolve_home(&Some("/tmp/fm-explicit".into()));
        assert_eq!(p, PathBuf::from("/tmp/fm-explicit"));
    }

    #[test]
    fn fallback_home_env() {
        std::env::set_var("HOME", "/tmp/fm-home-test");
        let p = resolve_home(&None);
        assert_eq!(p, PathBuf::from("/tmp/fm-home-test/.fusion-memory"));
        std::env::remove_var("HOME");
    }

    #[test]
    fn fallback_relative_when_no_home() {
        std::env::remove_var("HOME");
        let p = resolve_home(&None);
        assert_eq!(p, PathBuf::from(".fusion-memory"));
    }
}
