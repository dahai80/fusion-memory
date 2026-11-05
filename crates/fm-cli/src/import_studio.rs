//! import-agent-studio CLI entry. #33 (v1.5.0): implementation moved into
//! fm-engine::import_studio (shared with the fm-server HTTP import route); this
//! module keeps only CLI home-directory resolution and delegates. PRD §11.5.

pub use fm_engine::import_studio::run_import;

use crate::paths::resolve_home;
use fm_engine::import_studio::build_import_engine_at;
use fm_engine::MemoryEngine;

/// Build an import engine (home resolved with CLI semantics: explicit > ~/.fusion-memory).
pub fn build_import_engine(home: &Option<String>, stub: bool) -> Result<MemoryEngine, String> {
    build_import_engine_at(&resolve_home(home), stub)
}
