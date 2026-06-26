//! Rendering the skills catalog into the system prompt.
//!
//! Only skill metadata (name + description) is rendered; the body is loaded
//! lazily via the `read_skill` tool (progressive disclosure). The block is
//! plain Markdown so it works identically under the native, XML, and caret
//! tool dialects.

use crate::skills::loader::Skill;

/// Soft cap on the number of skills listed in the catalog.
const MAX_SHOWN: usize = 20;

/// Render the "Available Skills" system-prompt section for `project`, or
/// `None` when there are no skills to advertise.
///
/// The section is deliberately **static** for a given project (it does not
/// reflect which skills have been loaded), so the system prompt stays stable
/// across turns and remains eligible for provider prompt caching.
pub fn render_skills_section(project: &str, skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut out = String::from("# Available Skills\n\n");
    out.push_str(&format!(
        "The following skills are available. Each entry shows the skill's name, its scope (in \
         parentheses), and a short description. To load a skill's full instructions, call the \
         `read_skill` tool with the skill's name and its scope: pass the project name `{project}` \
         for `project` skills, `:config:` for `user` skills, and `:system:` for `system` skills.\n\n",
    ));
    out.push_str(
        "Use a skill only when the user's task clearly matches its description. Do not load \
         skills speculatively. Skills can also be browsed with `list_skills`.\n\n",
    );
    out.push_str("Available skills:\n");

    for skill in skills.iter().take(MAX_SHOWN) {
        out.push_str(&format!(
            "- {} ({}): {}\n",
            skill.name,
            skill.scope.label(),
            skill.description
        ));
    }

    let overflow = skills.len().saturating_sub(MAX_SHOWN);
    if overflow > 0 {
        out.push_str(&format!(
            "\n{overflow} more skill(s) are not shown here. Use `list_skills` (with a scope — \
             this project's name, `:config:`, or `:system:` — and an optional query) to browse \
             the rest.\n"
        ));
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            skill_md: PathBuf::from(format!(".agents/skills/{name}/SKILL.md")),
            dir: PathBuf::from(format!(".agents/skills/{name}")),
            scope: crate::skills::SkillScope::Project,
        }
    }

    #[test]
    fn empty_catalog_renders_nothing() {
        assert!(render_skills_section("my-project", &[]).is_none());
    }

    #[test]
    fn renders_skill_entries() {
        let skills = vec![
            skill("alpha", "First skill."),
            skill("beta", "Second skill."),
        ];
        let rendered = render_skills_section("my-project", &skills).expect("should render");
        assert!(rendered.contains("# Available Skills"));
        assert!(rendered.contains("read_skill"));
        assert!(rendered.contains("`my-project`"));
        assert!(rendered.contains("- alpha (project): First skill."));
        assert!(rendered.contains("- beta (project): Second skill."));
    }

    #[test]
    fn renders_scope_label() {
        let mut user_skill = skill("security-review", "Audit auth.");
        user_skill.scope = crate::skills::SkillScope::User;
        let rendered = render_skills_section("my-project", &[user_skill]).expect("should render");
        assert!(rendered.contains("- security-review (user): Audit auth."));
        // The legend explains how to address each scope.
        assert!(rendered.contains(":config:"));
        assert!(rendered.contains(":system:"));
    }

    #[test]
    fn renders_overflow_marker() {
        let skills: Vec<Skill> = (0..(MAX_SHOWN + 3))
            .map(|i| skill(&format!("skill-{i:02}"), "desc"))
            .collect();
        let rendered = render_skills_section("p", &skills).expect("should render");
        // Overflow is reported with a count and points at list_skills.
        assert!(rendered.contains("3 more skill(s) are not shown"));
        assert!(rendered.contains("list_skills"));
    }
}
