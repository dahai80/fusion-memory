//! M8 #49: standard export — JSONL + Markdown (wikilink graph projection).
//! M7 #48: procedural memory → SKILL.md rendering.
//!
//! Defense line 2 (hard constraint): every export path applies the redact
//! pipeline unconditionally — `FUSION_MEMORY_REDACT_PII=0` only covers the
//! commit/import ingestion path, never the egress path. What leaves the
//! service is redacted, no exceptions.
//!
//! Rendering is deterministic (template only, no model call — Rule 5).

use crate::redact::redact_text;
use fm_core::MemoryItem;

/// JSONL export: one redacted item per line (round-trippable via serde).
pub fn export_jsonl(items: &[MemoryItem]) -> String {
    let mut out = String::new();
    for item in items {
        let mut it = item.clone();
        it.content = redact_text(&item.content);
        for e in &mut it.entities {
            e.name = redact_text(&e.name);
        }
        // vector_ref is a local store handle — meaningless outside this node.
        it.vector_ref = String::new();
        if let Ok(line) = serde_json::to_string(&it) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Markdown export with wikilink graph projection (Basic Memory / memU
/// file-style boundary): each memory a section, entities as [[name]] links.
pub fn export_markdown(items: &[MemoryItem]) -> String {
    let mut out = String::from("# Fusion Memory Export\n\n");
    for item in items {
        let content = redact_text(&item.content);
        out.push_str(&format!(
            "## [[{}]] {}\n\n{}\n",
            item.id,
            memory_type_name(item),
            content
        ));
        if !item.entities.is_empty() {
            let links = item
                .entities
                .iter()
                .map(|e| format!("[[{}]]", redact_text(&e.name)))
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!("\nRelated: {links}\n"));
        }
        if !item.sources.is_empty() {
            out.push_str(&format!("\nSources: {}\n", item.sources.join(", ")));
        }
        if let Some(c) = item.confidence {
            out.push_str(&format!("\nConfidence: {c:.2}\n"));
        }
        if item.valid_from.is_some() || item.valid_until.is_some() {
            out.push_str(&format!(
                "\nValidity: {} .. {}\n",
                item.valid_from
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into()),
                item.valid_until
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into()),
            ));
        }
        out.push('\n');
    }
    out
}

fn memory_type_name(item: &MemoryItem) -> &'static str {
    match item.memory_type {
        fm_core::MemoryType::Episodic => "Episodic",
        fm_core::MemoryType::Semantic => "Semantic",
        fm_core::MemoryType::Procedural => "Procedural",
    }
}

/// One rendered SKILL.md (agent-studio / fusion-harness skills contract:
/// YAML frontmatter with name/description + markdown body).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillDoc {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub markdown: String,
}

/// Render one Procedural memory as a SKILL.md. Deterministic: name comes from
/// the first attached entity (falls back to the memory id), description is the
/// first content line, the body carries the full redacted content plus a
/// provenance footer (sources + confidence) so consumers can judge trust.
pub fn render_skill(item: &MemoryItem) -> SkillDoc {
    let content = redact_text(&item.content);
    let name = item
        .entities
        .first()
        .map(|e| redact_text(&e.name))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| item.id.clone());
    let description = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(&content)
        .trim()
        .chars()
        .take(160)
        .collect::<String>();
    let mut md = format!(
        "---\nname: {}\ndescription: {}\nversion: 1\nsource: fusion-memory\nlicense: Apache-2.0\n---\n\n# {}\n\n{}\n",
        yaml_safe(&name),
        yaml_safe(&description),
        name,
        content
    );
    if !item.sources.is_empty() {
        md.push_str("\n## Provenance\n\n");
        for s in &item.sources {
            md.push_str(&format!("- episode: {s}\n"));
        }
        if let Some(c) = item.confidence {
            md.push_str(&format!("- confidence: {c:.2}\n"));
        }
    }
    SkillDoc {
        id: item.id.clone(),
        name,
        confidence: item.confidence,
        markdown: md,
    }
}

/// Render Procedural items (in the given order) as SKILL.md docs.
pub fn render_skills(items: &[MemoryItem]) -> Vec<SkillDoc> {
    items.iter().map(render_skill).collect()
}

/// Minimal YAML scalar safety: quote values containing YAML-special chars.
fn yaml_safe(v: &str) -> String {
    if v.contains(':')
        || v.contains('#')
        || v.contains('\n')
        || v.starts_with(' ')
        || v.ends_with(' ')
        || v.starts_with('\'')
        || v.starts_with('"')
        || v.starts_with('[')
        || v.starts_with('{')
        || v.starts_with('&')
        || v.starts_with('*')
        || v.starts_with('!')
    {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, content: &str, mt: fm_core::MemoryType) -> MemoryItem {
        let mut it = MemoryItem::new_turn_skeleton(
            id.into(),
            "ix".into(),
            0,
            "s".into(),
            String::new(),
            mt,
            content.into(),
            1000,
        );
        it.sources = vec!["ix-a".into(), "ix-b".into()];
        it.confidence = Some(0.75);
        it
    }

    // M8 acceptance gate: export passes redact — PII must never leave raw.
    #[test]
    fn export_redact_assertion() {
        let it = item(
            "m1",
            "call the user at 13800138000 or email ops@example.com",
            fm_core::MemoryType::Episodic,
        );
        let jsonl = export_jsonl(std::slice::from_ref(&it));
        let md = export_markdown(&[it]);
        assert!(!jsonl.contains("13800138000"), "jsonl leaks phone: {jsonl}");
        assert!(!jsonl.contains("ops@example.com"), "jsonl leaks email");
        assert!(!md.contains("13800138000"), "md leaks phone: {md}");
        assert!(!md.contains("ops@example.com"), "md leaks email");
        assert!(jsonl.contains("[REDACTED:"), "redaction marker present");
    }

    #[test]
    fn markdown_wikilink_projection() {
        let mut it = item(
            "m2",
            "restart the pipeline",
            fm_core::MemoryType::Procedural,
        );
        it.entities.push(fm_core::EntityNode {
            id: "e1".into(),
            name: "deploy-bot".into(),
            aliases: vec![],
            entity_type: fm_core::EntityType::Tech,
        });
        let md = export_markdown(&[it]);
        assert!(md.contains("[[m2]]"), "memory wikilink: {md}");
        assert!(md.contains("[[deploy-bot]]"), "entity wikilink: {md}");
        assert!(md.contains("Sources: ix-a, ix-b"));
    }

    // M7 acceptance: SKILL.md artifact parses as frontmatter + body shape.
    #[test]
    fn skill_md_frontmatter_contract() {
        let mut it = item(
            "p1",
            "deploy steps\ncurl the endpoint",
            fm_core::MemoryType::Procedural,
        );
        it.entities.push(fm_core::EntityNode {
            id: "e2".into(),
            name: "deploy-bot".into(),
            aliases: vec![],
            entity_type: fm_core::EntityType::Tech,
        });
        let doc = render_skill(&it);
        assert_eq!(doc.name, "deploy-bot");
        let md = &doc.markdown;
        assert!(md.starts_with("---\n"), "frontmatter first: {md}");
        assert!(md.contains("name: deploy-bot"));
        assert!(md.contains("description: deploy steps"));
        assert!(md.contains("license: Apache-2.0"));
        // provenance footer
        assert!(md.contains("## Provenance"));
        assert!(md.contains("episode: ix-a"));
        assert!(md.contains("confidence: 0.75"));
    }

    #[test]
    fn skill_redact_applies() {
        let it = item(
            "p2",
            "ping 13800138000 then deploy",
            fm_core::MemoryType::Procedural,
        );
        let doc = render_skill(&it);
        assert!(!doc.markdown.contains("13800138000"), "skill leaks PII");
    }

    #[test]
    fn yaml_safe_quotes_colon_values() {
        assert_eq!(yaml_safe("plain-name"), "plain-name");
        assert_eq!(yaml_safe("a: b"), "\"a: b\"");
    }
}
