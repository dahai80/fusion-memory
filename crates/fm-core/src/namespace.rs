//! issue #21: 记忆作用域命名空间 (user / project / local)。
//!
//! 与 tenant 正交: tenant 隔离租户 (gateway X-Fusion-Tenant 权威), namespace 隔离
//! 同一租户内的记忆可见域 (消费方 fusion-agent-studio subagent `memory:` 字段)。
//! namespace 为空 = 全局 (旧行为, 向后兼容)。
//!
//! 可见性堆叠规则 (project ⊂ user, local 仅本机):
//!   user    -> 仅 user 同级 (key 可选精化)
//!   project -> project 同级 (key 精化) ∪ user
//!   local   -> 仅 local 同级 (key 精化), 不叠 user
//! 与 delete_scope/count 的 "scope" (session_id 语义) 是不同的维度, RPC 参数名为
//! namespace/namespace_key 以免混淆。

pub const NS_USER: &str = "user";
pub const NS_PROJECT: &str = "project";
pub const NS_LOCAL: &str = "local";

pub fn is_valid_namespace(ns: &str) -> bool {
    ns.is_empty() || ns == NS_USER || ns == NS_PROJECT || ns == NS_LOCAL
}

/// 检索/检索作用域过滤器。namespace="" = 不过滤 (全局, 旧行为)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NamespaceFilter {
    pub namespace: String,
    pub key: String,
}

impl NamespaceFilter {
    pub fn new(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            key: key.into(),
        }
    }

    pub fn is_active(&self) -> bool {
        !self.namespace.is_empty()
    }

    /// 单条记忆是否可见 (内存侧判定; SQL 侧见 fm-persist ns 谓词)。未知 namespace 一律不可见 (fail-closed)。
    pub fn visible(&self, ns: &str, key: &str) -> bool {
        if !self.is_active() {
            return true;
        }
        let key_ok = self.key.is_empty() || key == self.key;
        match self.namespace.as_str() {
            NS_USER => ns == NS_USER && key_ok,
            NS_PROJECT => (ns == NS_PROJECT && key_ok) || ns == NS_USER,
            NS_LOCAL => ns == NS_LOCAL && key_ok,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_namespace_is_global() {
        let f = NamespaceFilter::default();
        assert!(!f.is_active());
        assert!(f.visible("project", "k"));
        assert!(f.visible("", ""));
    }

    #[test]
    fn project_stacks_user() {
        let f = NamespaceFilter::new(NS_PROJECT, "p1");
        assert!(f.visible(NS_PROJECT, "p1"));
        assert!(f.visible(NS_USER, "u1"));
        assert!(!f.visible(NS_PROJECT, "p2"));
        assert!(!f.visible(NS_LOCAL, "p1"));
        assert!(!f.visible("", ""));
    }

    #[test]
    fn local_does_not_stack_user() {
        let f = NamespaceFilter::new(NS_LOCAL, "m1");
        assert!(f.visible(NS_LOCAL, "m1"));
        assert!(!f.visible(NS_USER, "u1"));
        assert!(!f.visible(NS_PROJECT, "m1"));
    }

    #[test]
    fn user_ignores_key_when_empty() {
        let f = NamespaceFilter::new(NS_USER, "");
        assert!(f.visible(NS_USER, "any"));
        assert!(!f.visible(NS_PROJECT, "any"));
    }

    #[test]
    fn unknown_namespace_fail_closed() {
        let f = NamespaceFilter::new("bogus", "k");
        assert!(!f.visible(NS_USER, "k"));
        assert!(!NamespaceFilter::new(NS_PROJECT, "").visible("bogus", ""));
    }

    #[test]
    fn validation() {
        assert!(is_valid_namespace(""));
        assert!(is_valid_namespace(NS_USER));
        assert!(is_valid_namespace(NS_PROJECT));
        assert!(is_valid_namespace(NS_LOCAL));
        assert!(!is_valid_namespace("global"));
    }
}
