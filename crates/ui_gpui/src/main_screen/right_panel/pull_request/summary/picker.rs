//! The popover list behind the Reviewers and Labels rows: a search field
//! over the candidates the host offers, the requested or applied ones
//! checked. A click asks the Summary tab to toggle one
//! ([`PickerToggle`]); the tab writes and ticks it when that worked.

use super::super::avatar::avatar;
use github::model::{Actor, Reviewer};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Sizable, Size};
use gpui_kit::{
    Context, Entity, EventEmitter, Hsla, Render, SharedString, Subscription, Window, div,
    prelude::*, px, svg,
};

/// What a picker row stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Candidate {
    Reviewer(Reviewer),
    Label(String),
}

/// What leads a row.
#[derive(Clone, Debug)]
pub enum Leading {
    Avatar(Actor),
    /// A label's colour; `None` for one without.
    Dot(Option<Hsla>),
}

#[derive(Clone, Debug)]
pub struct PickerEntry {
    pub candidate: Candidate,
    pub label: SharedString,
    /// A name, a description, "team".
    pub detail: Option<SharedString>,
    pub leading: Leading,
    pub checked: bool,
}

impl PickerEntry {
    /// The search matches the label or the detail, ignoring case.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || self.label.to_lowercase().contains(&query)
            || self
                .detail
                .as_ref()
                .is_some_and(|d| d.to_lowercase().contains(&query))
    }
}

/// The texts of one kind of picker.
#[derive(Clone, Copy)]
pub struct PickerTexts {
    pub search: &'static str,
    pub empty: &'static str,
    pub no_match: &'static str,
    pub failed: &'static str,
    pub truncated: &'static str,
}

pub const REVIEWER_TEXTS: PickerTexts = PickerTexts {
    search: "Search people with access",
    empty: "Nobody else has access to this repository.",
    no_match: "Nobody with access matches that.",
    failed: "The people with access could not be read.",
    truncated: "More people have access than are listed here.",
};

pub const LABEL_TEXTS: PickerTexts = PickerTexts {
    search: "Search labels",
    empty: "This repository has no labels.",
    no_match: "No label matches that.",
    failed: "The labels could not be read.",
    truncated: "The repository has more labels than are listed here.",
};

/// Toggle `0`: request or apply it when `1`, else take it back.
pub struct PickerToggle(pub Candidate, pub bool);

enum Candidates {
    Loading,
    Failed,
    Read {
        entries: Vec<PickerEntry>,
        truncated: bool,
    },
}

pub struct CandidatePicker {
    texts: PickerTexts,
    search: Entity<InputState>,
    query: String,
    candidates: Candidates,
    /// A toggle is being written.
    busy: bool,
    _search: Subscription,
}

impl EventEmitter<PickerToggle> for CandidatePicker {}

impl CandidatePicker {
    pub fn new(texts: PickerTexts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(texts.search));
        search.update(cx, |search, cx| search.focus(window, cx));
        let subscription = cx.subscribe(&search, |this, search, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = search.read(cx).value().to_string();
                cx.notify();
            }
        });
        Self {
            texts,
            search,
            query: String::new(),
            candidates: Candidates::Loading,
            busy: false,
            _search: subscription,
        }
    }

    pub fn set_entries(
        &mut self,
        entries: Vec<PickerEntry>,
        truncated: bool,
        cx: &mut Context<Self>,
    ) {
        self.candidates = Candidates::Read { entries, truncated };
        cx.notify();
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[PickerEntry] {
        match &self.candidates {
            Candidates::Read { entries, .. } => entries,
            Candidates::Loading | Candidates::Failed => &[],
        }
    }

    pub fn set_failed(&mut self, cx: &mut Context<Self>) {
        self.candidates = Candidates::Failed;
        cx.notify();
    }

    pub fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.busy = busy;
        cx.notify();
    }

    /// The write for `candidate` went through.
    pub fn set_checked(&mut self, candidate: &Candidate, checked: bool, cx: &mut Context<Self>) {
        if let Candidates::Read { entries, .. } = &mut self.candidates
            && let Some(entry) = entries.iter_mut().find(|e| e.candidate == *candidate)
        {
            entry.checked = checked;
            cx.notify();
        }
    }

    fn render_entry(
        &self,
        ix: usize,
        entry: &PickerEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let leading = match &entry.leading {
            Leading::Avatar(actor) => avatar(Some(actor), px(18.), cx),
            Leading::Dot(color) => div()
                .flex_none()
                .size_2()
                .rounded_full()
                .bg(color.unwrap_or(muted))
                .into_any_element(),
        };
        let candidate = entry.candidate.clone();
        let checked = entry.checked;
        div()
            .id(("picker-entry", ix))
            .px_2()
            .py_1()
            .rounded_md()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .when(!self.busy, |row| {
                row.cursor_pointer()
                    .hover(|s| s.bg(theme.muted))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(PickerToggle(candidate.clone(), !checked))
                    }))
            })
            .child(leading)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.foreground)
                            .child(entry.label.clone()),
                    )
                    .when_some(entry.detail.clone(), |el, detail| {
                        el.child(div().min_w_0().truncate().text_color(muted).child(detail))
                    }),
            )
            .when(checked, |row| {
                row.child(
                    svg()
                        .flex_none()
                        .size(px(13.))
                        .path("icons/check.svg")
                        .text_color(theme.foreground),
                )
            })
    }
}

impl Render for CandidatePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let note = |text: &'static str| div().px_2().py_1().text_xs().text_color(muted).child(text);
        let body = match &self.candidates {
            Candidates::Loading => note("Loading…").into_any_element(),
            Candidates::Failed => note(self.texts.failed).into_any_element(),
            Candidates::Read { entries, truncated } => {
                let shown: Vec<(usize, PickerEntry)> = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.matches(&self.query))
                    .map(|(i, e)| (i, e.clone()))
                    .collect();
                let truncated = *truncated;
                div()
                    .flex()
                    .flex_col()
                    .when(entries.is_empty(), |el| el.child(note(self.texts.empty)))
                    .when(!entries.is_empty() && shown.is_empty(), |el| {
                        el.child(note(self.texts.no_match))
                    })
                    .children(
                        shown
                            .iter()
                            .map(|(ix, entry)| self.render_entry(*ix, entry, cx)),
                    )
                    .when(truncated, |el| el.child(note(self.texts.truncated)))
                    .into_any_element()
            }
        };
        div()
            .w(px(280.))
            .flex()
            .flex_col()
            .gap_1()
            .child(Input::new(&self.search).with_size(Size::Small))
            .child(
                div()
                    .id("picker-candidates")
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, detail: Option<&str>) -> PickerEntry {
        PickerEntry {
            candidate: Candidate::Label(label.into()),
            label: label.to_owned().into(),
            detail: detail.map(|d| d.to_owned().into()),
            leading: Leading::Dot(None),
            checked: false,
        }
    }

    #[test]
    fn the_search_matches_label_or_detail() {
        let bug = entry("bug", Some("Something isn't working"));
        assert!(bug.matches(""));
        assert!(bug.matches(" BU "));
        assert!(bug.matches("working"));
        assert!(!bug.matches("docs"));
        assert!(!entry("docs", None).matches("bug"));
    }
}
