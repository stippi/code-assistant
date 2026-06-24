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
/// `None` when there are no skills to advertise. Skills whose name appears in
/// `active` (already loaded via `read_skill` on the current branch) are listed
/// separately so the model does not reload them needlessly.
pub fn render_skills_section(project: &str, skills: &[Skill], active: &[String]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let (active_skills, available_skills): (Vec<&Skill>, Vec<&Skill>) = skills
        .iter()
        .partition(|skill| active.iter().any(|name| name == &skill.name));

    let mut out = String::from("# Available Skills\n\n");
    out.push_str(&format!(
        "The following skills are available in project `{project}`. Each entry shows the skill's \
         name and a short description. To use a skill, call the `read_skill` tool with this \
         project name and the skill's name to load its full instructions into the conversation.\n\n",
    ));
    out.push_str(
        "Use a skill only when the user's task clearly matches its description. Do not load \
         skills speculatively. Skills in other projects can be browsed with `list_skills`.\n\n",
    );

    if !active_skills.is_empty() {
        out.push_str(
            "Active skills (loaded earlier — their instructions should already be in context; \
             only call `read_skill` again if you can no longer see them):\n",
        );
        for skill in &active_skills {
            out.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        out.push('\n');
    }

    if !available_skills.is_empty() {
        out.push_str("Available skills:\n");
        for skill in available_skills.iter().take(MAX_SHOWN) {
            out.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        let overflow = available_skills.len().saturating_sub(MAX_SHOWN);
        if overflow > 0 {
            out.push_str(&format!("- (+{overflow} more available)\n"));
        }
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
        }
    }

    #[test]
    fn empty_catalog_renders_nothing() {
        assert!(render_skills_section("my-project", &[], &[]).is_none());
    }

    #[test]
    fn renders_skill_entries() {
        let skills = vec![
            skill("alpha", "First skill."),
            skill("beta", "Second skill."),
        ];
        let rendered = render_skills_section("my-project", &skills, &[]).expect("should render");
        assert!(rendered.contains("# Available Skills"));
        assert!(rendered.contains("read_skill"));
        assert!(rendered.contains("`my-project`"));
        assert!(rendered.contains("- alpha: First skill."));
        assert!(rendered.contains("- beta: Second skill."));
        // With no active skills, the "Active skills" header is omitted.
        assert!(!rendered.contains("Active skills"));
    }

    #[test]
    fn partitions_active_skills() {
        let skills = vec![
            skill("alpha", "First skill."),
            skill("beta", "Second skill."),
        ];
        let active = vec!["alpha".to_string()];
        let rendered =
            render_skills_section("my-project", &skills, &active).expect("should render");

        let active_idx = rendered.find("Active skills").expect("active header");
        let available_idx = rendered
            .find("Available skills:")
            .expect("available header");
        // Active section comes before the available list.
        assert!(active_idx < available_idx);
        // alpha is listed under active, beta under available.
        let active_part = &rendered[active_idx..available_idx];
        assert!(active_part.contains("- alpha: First skill."));
        assert!(!active_part.contains("- beta:"));
        assert!(rendered[available_idx..].contains("- beta: Second skill."));
    }

    #[test]
    fn renders_overflow_marker() {
        let skills: Vec<Skill> = (0..(MAX_SHOWN + 3))
            .map(|i| skill(&format!("skill-{i:02}"), "desc"))
            .collect();
        let rendered = render_skills_section("p", &skills, &[]).expect("should render");
        assert!(rendered.contains("(+3 more available)"));
    }
}
