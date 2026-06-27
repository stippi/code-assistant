//! Anthropic-compatible Agent Skills.
//!
//! A skill is a directory containing a `SKILL.md` (YAML frontmatter + Markdown
//! body), optionally bundled with `scripts/`, `references/`, and `assets/`.
//! Skills follow a *progressive disclosure* model: only metadata (name +
//! description) is placed in the system prompt; the full body is loaded on
//! demand via the `read_skill` tool.
//!
//! This initial slice covers project-scoped discovery, catalog rendering, and
//! on-demand loading. User/system scopes, bundled skills, configuration, and
//! session-level activation tracking are deferred.

pub mod bundled;
pub mod config;
pub mod loader;
pub mod manifest;
pub mod render;

pub use bundled::install_system_skills;
pub use config::{skills_config_path, SkillsConfig};
pub use loader::{discover_all_skills, discover_scope_skills, ScopeSkills, Skill, SkillScope};
pub use manifest::{parse_skill_content, SkillManifest};
pub use render::render_skills_section;
