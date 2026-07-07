use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, Focusable, Render, Window};
use gpui_component::{
    select::{Select, SelectEvent, SelectItem, SelectState},
    ActiveTheme, Icon, Sizable, Size,
};
use tools_core::permissions::PermissionTier;

#[derive(Clone, Debug)]
pub enum PermissionSelectorEvent {
    TierChanged { tier: PermissionTier },
}

#[derive(Clone, Debug)]
struct PermissionOption {
    label: &'static str,
    tier: PermissionTier,
}

impl PermissionOption {
    fn new(label: &'static str, tier: PermissionTier) -> Self {
        Self { label, tier }
    }
}

impl SelectItem for PermissionOption {
    type Value = PermissionTier;

    fn title(&self) -> gpui::SharedString {
        self.label.into()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        None
    }

    fn value(&self) -> &Self::Value {
        &self.tier
    }
}

pub struct PermissionSelector {
    dropdown_state: Entity<SelectState<Vec<PermissionOption>>>,
    _subscription: gpui::Subscription,
}

impl EventEmitter<PermissionSelectorEvent> for PermissionSelector {}

impl PermissionSelector {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let options = Self::options();
        let default_tier = PermissionTier::default();
        let dropdown_state =
            cx.new(|cx| SelectState::new(Vec::<PermissionOption>::new(), None, window, cx));

        dropdown_state.update(cx, |state, cx| {
            state.set_items(options, window, cx);
            state.set_selected_value(&default_tier, window, cx);
        });

        let subscription = cx.subscribe_in(&dropdown_state, window, Self::on_dropdown_event);

        Self {
            dropdown_state,
            _subscription: subscription,
        }
    }

    fn options() -> Vec<PermissionOption> {
        vec![
            PermissionOption::new("Bypass Permissions", PermissionTier::BypassAll),
            PermissionOption::new("Ask Before Writes", PermissionTier::WriteTools),
            PermissionOption::new("Ask For All Tools", PermissionTier::AllTools),
        ]
    }

    fn on_dropdown_event(
        &mut self,
        _: &Entity<SelectState<Vec<PermissionOption>>>,
        event: &SelectEvent<Vec<PermissionOption>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(tier)) = event {
            cx.emit(PermissionSelectorEvent::TierChanged { tier: *tier });
        }
    }

    pub fn set_tier(&mut self, tier: PermissionTier, window: &mut Window, cx: &mut Context<Self>) {
        self.dropdown_state.update(cx, |state, cx| {
            state.set_selected_value(&tier, window, cx);
        });
    }
}

impl Focusable for PermissionSelector {
    fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.dropdown_state.focus_handle(cx)
    }
}

impl Render for PermissionSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().text_color(cx.theme().muted_foreground).child(
            Select::new(&self.dropdown_state)
                .placeholder("Permissions")
                .with_size(Size::XSmall)
                .appearance(false)
                .icon(
                    Icon::default()
                        .path("icons/chevron_up_down.svg")
                        .with_size(Size::XSmall)
                        .text_color(cx.theme().muted_foreground),
                )
                .min_w(px(130.)),
        )
    }
}
