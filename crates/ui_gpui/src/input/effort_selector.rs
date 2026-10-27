//! Reasoning effort selector next to the model selector. Offers the levels
//! the session's model supports and hides itself for models that do not
//! reason.

use code_assistant_core::session::effort::EffortChoice;
use gpui_kit::component::{
    ActiveTheme, Icon, Sizable, Size,
    select::{Select, SelectEvent, SelectItem, SelectState},
};
use gpui_kit::{Context, Entity, EventEmitter, Focusable, Render, Window, div, prelude::*, px};
use llm::capabilities::Effort;

#[derive(Clone, Debug)]
pub enum EffortSelectorEvent {
    EffortChanged { effort: Effort },
}

#[derive(Clone, Debug)]
struct EffortOption(Effort);

impl SelectItem for EffortOption {
    type Value = Effort;

    fn title(&self) -> gpui_kit::SharedString {
        self.0.label().into()
    }

    /// The selected level, led by the brain icon (the menu lists plain labels).
    fn display_title(&self) -> Option<gpui_kit::AnyElement> {
        Some(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    Icon::default()
                        .path("icons/brain.svg")
                        .with_size(Size::XSmall),
                )
                .child(self.0.label())
                .into_any_element(),
        )
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

pub struct EffortSelector {
    dropdown_state: Entity<SelectState<Vec<EffortOption>>>,
    choice: EffortChoice,
    _subscription: gpui_kit::Subscription,
}

impl EventEmitter<EffortSelectorEvent> for EffortSelector {}

impl EffortSelector {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dropdown_state =
            cx.new(|cx| SelectState::new(Vec::<EffortOption>::new(), None, window, cx));
        let subscription = cx.subscribe_in(&dropdown_state, window, Self::on_dropdown_event);
        Self {
            dropdown_state,
            choice: EffortChoice::default(),
            _subscription: subscription,
        }
    }

    fn on_dropdown_event(
        &mut self,
        _: &Entity<SelectState<Vec<EffortOption>>>,
        event: &SelectEvent<Vec<EffortOption>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(effort)) = event
            && self.choice.selected != Some(*effort)
        {
            self.choice.selected = Some(*effort);
            cx.emit(EffortSelectorEvent::EffortChanged { effort: *effort });
        }
    }

    pub fn choice(&self) -> &EffortChoice {
        &self.choice
    }

    pub fn set_choice(
        &mut self,
        choice: EffortChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if choice == self.choice {
            return;
        }
        let items = choice.levels.iter().copied().map(EffortOption).collect();
        self.dropdown_state.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if let Some(selected) = choice.selected {
                state.set_selected_value(&selected, window, cx);
            }
        });
        self.choice = choice;
        cx.notify();
    }
}

impl Focusable for EffortSelector {
    fn focus_handle(&self, cx: &gpui_kit::App) -> gpui_kit::FocusHandle {
        self.dropdown_state.focus_handle(cx)
    }
}

impl Render for EffortSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.choice.levels.is_empty() {
            return div().into_any_element();
        }
        div()
            .flex_none()
            .text_color(cx.theme().muted_foreground)
            .child(
                Select::new(&self.dropdown_state)
                    .placeholder("Effort")
                    .with_size(Size::XSmall)
                    .appearance(false)
                    .icon(
                        Icon::default()
                            .path("icons/chevron_up_down.svg")
                            .with_size(Size::XSmall)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .menu_width(px(140.)),
            )
            .into_any_element()
    }
}
