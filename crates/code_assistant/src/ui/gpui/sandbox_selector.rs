use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, Focusable, Render, Window};
use gpui_component::{
    select::{Select, SelectEvent, SelectItem, SelectState},
    ActiveTheme, Icon, Sizable, Size,
};
use sandbox::SandboxPolicy;

#[derive(Clone, Debug)]
pub enum SandboxSelectorEvent {
    PolicyChanged { policy: SandboxPolicy },
}

#[derive(Clone, Debug)]
struct SandboxOption {
    label: &'static str,
    description: &'static str,
    policy: SandboxPolicy,
}

impl SandboxOption {
    fn new(label: &'static str, description: &'static str, policy: SandboxPolicy) -> Self {
        Self {
            label,
            description,
            policy,
        }
    }
}

impl SelectItem for SandboxOption {
    type Value = SandboxPolicy;

    fn title(&self) -> gpui::SharedString {
        self.label.into()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        let row = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(self.label))
            .child(div().text_xs().child(self.description));
        Some(row.into_any_element())
    }

    fn value(&self) -> &Self::Value {
        &self.policy
    }
}

pub struct SandboxSelector {
    dropdown_state: Entity<SelectState<Vec<SandboxOption>>>,
    _subscription: gpui::Subscription,
}

impl EventEmitter<SandboxSelectorEvent> for SandboxSelector {}

impl SandboxSelector {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let options = Self::options();
        let default_policy = SandboxPolicy::DangerFullAccess;
        let dropdown_state =
            cx.new(|cx| SelectState::new(Vec::<SandboxOption>::new(), None, window, cx));

        dropdown_state.update(cx, |state, cx| {
            state.set_items(options, window, cx);
            state.set_selected_value(&default_policy, window, cx);
        });

        let subscription = cx.subscribe_in(&dropdown_state, window, Self::on_dropdown_event);

        Self {
            dropdown_state,
            _subscription: subscription,
        }
    }

    fn options() -> Vec<SandboxOption> {
        vec![
            SandboxOption::new(
                "Danger Full Access",
                "No sandboxing; full system access (default)",
                SandboxPolicy::DangerFullAccess,
            ),
            SandboxOption::new(
                "Read Only",
                "Read-only filesystem; commands cannot modify files",
                SandboxPolicy::ReadOnly,
            ),
            SandboxOption::new(
                "Workspace Write",
                "Writes limited to session workspace and temp dirs",
                SandboxPolicy::WorkspaceWrite {
                    writable_roots: Vec::new(),
                    network_access: false,
                    exclude_tmpdir_env_var: false,
                    exclude_slash_tmp: false,
                },
            ),
        ]
    }

    fn on_dropdown_event(
        &mut self,
        _: &Entity<SelectState<Vec<SandboxOption>>>,
        event: &SelectEvent<Vec<SandboxOption>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(policy)) = event {
            cx.emit(SandboxSelectorEvent::PolicyChanged {
                policy: policy.clone(),
            });
        }
    }

    pub fn set_policy(
        &mut self,
        policy: SandboxPolicy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dropdown_state.update(cx, |state, cx| {
            state.set_selected_value(&policy, window, cx);
        });
    }
}

impl Focusable for SandboxSelector {
    fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.dropdown_state.focus_handle(cx)
    }
}

impl Render for SandboxSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().text_color(cx.theme().muted_foreground).child(
            Select::new(&self.dropdown_state)
                .placeholder("Sandbox Mode")
                .with_size(Size::XSmall)
                .appearance(false)
                .icon(
                    Icon::default()
                        .path("icons/shield_check.svg")
                        .with_size(Size::XSmall)
                        .text_color(cx.theme().muted_foreground),
                )
                .min_w(px(180.))
                .max_w(px(280.)),
        )
    }
}
