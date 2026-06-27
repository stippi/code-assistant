//! Skills settings section — master toggle, bundled-skills toggle, and a
//! per-skill enable/disable list. Backed by `<config_dir>/skills.json`.

use code_assistant_core::skills::{
    discover_config_and_system_skills, install_system_skills, Skill, SkillsConfig,
};
use gpui::{div, prelude::*, px, App, Context, FocusHandle, Focusable, SharedString};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Disableable, Sizable, Size};
use tracing::warn;

pub struct SkillsSection {
    focus_handle: FocusHandle,
    config: SkillsConfig,
    /// User (`:config:`) and system (`:system:`) skills, unfiltered, so
    /// disabled skills still appear with their toggle state.
    skills: Vec<Skill>,
}

impl SkillsSection {
    pub fn new(_window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            config: SkillsConfig::load(),
            skills: discover_config_and_system_skills(),
        }
    }

    /// Reload config and discovered skills from disk.
    pub fn reload(&mut self) {
        self.config = SkillsConfig::load();
        self.skills = discover_config_and_system_skills();
    }

    /// Persist the current config and refresh the bundled tree + skill list.
    fn persist(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.config.save() {
            warn!("Failed to save skills.json: {e:#}");
        }
        // Re-extract or remove bundled skills to match the toggle.
        if let Err(e) = install_system_skills() {
            warn!("Failed to refresh bundled skills: {e:#}");
        }
        self.reload();
        cx.notify();
    }

    fn set_enabled(&mut self, value: bool, cx: &mut Context<Self>) {
        self.config.enabled = value;
        self.persist(cx);
    }

    fn set_bundled_enabled(&mut self, value: bool, cx: &mut Context<Self>) {
        self.config.bundled_skills_enabled = value;
        self.persist(cx);
    }

    fn toggle_skill(&mut self, skill: &Skill, cx: &mut Context<Self>) {
        if self.config.is_skill_disabled(skill) {
            // Re-enable: drop any entry matching the name or the SKILL.md path.
            let md = skill.skill_md.to_string_lossy().to_string();
            self.config
                .disabled
                .retain(|entry| entry != &skill.name && entry != &md);
        } else {
            self.config.disabled.push(skill.name.clone());
        }
        self.persist(cx);
    }

    fn render_toggle_row(
        &self,
        id: &'static str,
        label: &str,
        description: &str,
        checked: bool,
        disabled: bool,
        cx: &mut Context<Self>,
        on_toggle: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        let view = cx.entity();
        let on_toggle = std::rc::Rc::new(on_toggle);
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(label.to_string())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(description.to_string())),
                    ),
            )
            .child(
                Switch::new(id)
                    .checked(checked)
                    .disabled(disabled)
                    .with_size(Size::Small)
                    .on_click(move |new_value, _window, app| {
                        let new_value = *new_value;
                        let on_toggle = on_toggle.clone();
                        let _ = view.update(app, |this, cx| {
                            on_toggle(this, new_value, cx);
                        });
                    }),
            )
    }

    fn render_skill_row(&self, skill: &Skill, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let skill_for_click = skill.clone();
        let enabled = !self.config.is_skill_disabled(skill);
        let master_disabled = !self.config.enabled;
        let switch_id = SharedString::from(format!("skill-{}-{}", skill.scope.label(), skill.name));

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(SharedString::from(skill.name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(format!(
                                        "({})",
                                        skill.scope.label()
                                    ))),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(skill.description.clone())),
                    ),
            )
            .child(
                Switch::new(switch_id)
                    .checked(enabled)
                    .disabled(master_disabled)
                    .with_size(Size::Small)
                    .on_click(move |_new_value, _window, app| {
                        let skill = skill_for_click.clone();
                        let _ = view.update(app, |this, cx| {
                            this.toggle_skill(&skill, cx);
                        });
                    }),
            )
    }
}

impl Focusable for SkillsSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SkillsSection {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.config.enabled;
        let bundled_enabled = self.config.bundled_skills_enabled;
        let skills = self.skills.clone();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .max_w(px(700.))
            .mx_auto()
            // Header
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("SKILLS"),
            )
            // Toggles card
            .child(
                div()
                    .flex()
                    .flex_col()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .child(self.render_toggle_row(
                        "skills-enabled",
                        "Enable skills",
                        "Advertise skills in the system prompt and allow the read_skill / \
                         list_skills tools.",
                        enabled,
                        false,
                        cx,
                        |this, value, cx| this.set_enabled(value, cx),
                    ))
                    .child(self.render_toggle_row(
                        "skills-bundled",
                        "Use bundled skills",
                        "Extract and advertise the skills shipped with the app (system scope).",
                        bundled_enabled,
                        !enabled,
                        cx,
                        |this, value, cx| this.set_bundled_enabled(value, cx),
                    )),
            )
            // Per-skill list header
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child("USER & SYSTEM SKILLS"),
            )
            // Skill list / empty state
            .when(skills.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .py_8()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No user or system skills found"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(format!(
                                    "Add skills under {}/skills",
                                    code_assistant_core::config_dir::config_dir().display()
                                ))),
                        ),
                )
            })
            .when(!skills.is_empty(), |el| {
                el.child(
                    div().flex().flex_col().gap_2().children(
                        skills
                            .iter()
                            .map(|skill| self.render_skill_row(skill, cx).into_any_element()),
                    ),
                )
            })
    }
}
