//! The Markdown box every comment is written in: a new comment, a reply,
//! an edit, the description. Write | Preview (rendered as GitHub renders
//! it), ⌘↵ submits, Escape cancels. The owner carries out the submit and
//! says when it is under way ([`MarkdownEditor::set_busy`]); a failed
//! write keeps the text.

use super::super::markdown;
use github::RepoRef;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Render, SharedString,
    Subscription, Window, div, prelude::*,
};

pub enum EditorEvent {
    /// The trimmed text, to be sent.
    Submit(String),
    Cancel,
}

/// What an editor is for.
#[derive(Clone, Copy, Debug)]
pub struct EditorOptions {
    pub placeholder: &'static str,
    pub submit_label: &'static str,
    /// Submitting nothing is allowed (clearing a description).
    pub allow_empty: bool,
    pub min_rows: usize,
    pub max_rows: usize,
}

impl EditorOptions {
    /// The composer's.
    #[allow(dead_code, reason = "the composer's box, wired by the composer")]
    pub const COMMENT: EditorOptions = EditorOptions {
        placeholder: "Leave a comment",
        submit_label: "Comment",
        allow_empty: false,
        min_rows: 3,
        max_rows: 16,
    };
    pub const REPLY: EditorOptions = EditorOptions {
        placeholder: "Reply",
        submit_label: "Reply",
        allow_empty: false,
        min_rows: 2,
        max_rows: 12,
    };
    pub const EDIT: EditorOptions = EditorOptions {
        placeholder: "Edit comment",
        submit_label: "Save",
        allow_empty: false,
        min_rows: 3,
        max_rows: 20,
    };
    pub const DESCRIPTION: EditorOptions = EditorOptions {
        placeholder: "Describe this pull request",
        submit_label: "Save",
        allow_empty: true,
        min_rows: 6,
        max_rows: 24,
    };
}

pub struct MarkdownEditor {
    repo: RepoRef,
    options: EditorOptions,
    input: Entity<TextareaState>,
    /// The rendered draft, while previewing.
    preview: Option<Entity<TextViewState>>,
    previewing: bool,
    /// Holds the keyboard while previewing, for ⌘↵ and Escape.
    preview_focus: FocusHandle,
    busy: bool,
    _subscription: Subscription,
}

impl EventEmitter<EditorEvent> for MarkdownEditor {}

impl MarkdownEditor {
    /// An editor holding `text`, focused. `repo` resolves references in
    /// the preview.
    pub fn new(
        repo: RepoRef,
        text: &str,
        options: EditorOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = text.to_owned();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(options.min_rows, options.max_rows)
                .placeholder(options.placeholder)
                .default_value(text)
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter {
                secondary: true, ..
            } = event
            {
                this.submit(cx);
            }
        });
        Self {
            repo,
            options,
            input,
            preview: None,
            previewing: false,
            preview_focus: cx.focus_handle(),
            busy: false,
            _subscription: subscription,
        }
    }

    pub fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// A submit is under way: no edits, no second submit.
    pub fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        if self.busy != busy {
            self.busy = busy;
            cx.notify();
        }
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx).trim().to_owned();
        if self.busy || (text.is_empty() && !self.options.allow_empty) {
            return;
        }
        cx.emit(EditorEvent::Submit(text));
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(EditorEvent::Cancel);
        }
    }

    fn set_previewing(&mut self, previewing: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.previewing == previewing {
            return;
        }
        self.previewing = previewing;
        if previewing {
            let prepared = markdown::prepare(&self.text(cx), &self.repo);
            match &self.preview {
                Some(state) => state.update(cx, |state, cx| state.set_text(&prepared, cx)),
                None => self.preview = Some(cx.new(|cx| TextViewState::markdown(&prepared, cx))),
            }
            self.preview_focus.focus(window, cx);
        } else {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }
}

impl Focusable for MarkdownEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for MarkdownEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = cx.entity_id().as_u64();
        let empty = self.text(cx).trim().is_empty();
        let can_submit = !self.busy && (!empty || self.options.allow_empty);
        let mode = |label: &'static str, previewing: bool, selected: bool| {
            Button::new(SharedString::from(format!("md-editor-{id}-{label}")))
                .ghost()
                .xsmall()
                .label(label)
                .selected(selected)
                .disabled(self.busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_previewing(previewing, window, cx)
                }))
        };
        let body = if self.previewing {
            div()
                .min_h_16()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .child(match (&self.preview, empty) {
                    (Some(preview), false) => TextView::new(preview).into_any_element(),
                    _ => div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Nothing to preview.")
                        .into_any_element(),
                })
                .into_any_element()
        } else {
            Textarea::new(&self.input)
                .disabled(self.busy)
                .into_any_element()
        };
        div()
            .debug_selector(move || format!("md-editor-{id}"))
            .flex()
            .flex_col()
            .gap_1p5()
            .track_focus(&self.preview_focus)
            // The text area has the keys while writing; these are for the
            // preview.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if !this.previewing {
                    return;
                }
                let keystroke = &event.keystroke;
                match keystroke.key.as_str() {
                    "enter" if keystroke.modifiers.platform => this.submit(cx),
                    "escape" => this.cancel(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.cancel(cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .child(mode("Write", false, !self.previewing))
                    .child(mode("Preview", true, self.previewing)),
            )
            .child(body)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("md-editor-{id}-cancel")))
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("md-editor-{id}-submit")))
                            .primary()
                            .xsmall()
                            .label(self.options.submit_label)
                            .tooltip("⌘↵")
                            .loading(self.busy)
                            .disabled(!can_submit)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::repo;
    use gpui_kit::{TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug, PartialEq)]
    enum Seen {
        Submit(String),
        Cancel,
    }

    fn editor<'a>(
        text: &str,
        options: EditorOptions,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<MarkdownEditor>,
        Rc<RefCell<Vec<Seen>>>,
        &'a mut VisualTestContext,
    ) {
        let text = text.to_owned();
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| MarkdownEditor::new(repo(), &text, options, window, cx))
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        cx.update(|_, cx| {
            cx.subscribe(&view, move |_, event: &EditorEvent, _| {
                sink.borrow_mut().push(match event {
                    EditorEvent::Submit(text) => Seen::Submit(text.clone()),
                    EditorEvent::Cancel => Seen::Cancel,
                })
            })
            .detach()
        });
        cx.run_until_parked();
        (view, seen, cx)
    }

    #[gpui_kit::test]
    fn cmd_enter_submits_the_trimmed_text(cx: &mut TestAppContext) {
        let (_, seen, cx) = editor("  Looks good  ", EditorOptions::REPLY, cx);
        cx.simulate_keystrokes("cmd-enter");
        assert_eq!(
            seen.borrow().as_slice(),
            [Seen::Submit("Looks good".into())]
        );
    }

    #[gpui_kit::test]
    fn cmd_enter_and_escape_work_while_previewing(cx: &mut TestAppContext) {
        let (view, seen, cx) = editor("Looks good", EditorOptions::REPLY, cx);
        view.update_in(cx, |editor, window, cx| {
            editor.set_previewing(true, window, cx)
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-enter");
        cx.simulate_keystrokes("escape");
        assert_eq!(
            seen.borrow().as_slice(),
            [Seen::Submit("Looks good".into()), Seen::Cancel]
        );
    }

    #[gpui_kit::test]
    fn escape_cancels(cx: &mut TestAppContext) {
        let (_, seen, cx) = editor("draft", EditorOptions::REPLY, cx);
        cx.simulate_keystrokes("escape");
        assert_eq!(seen.borrow().as_slice(), [Seen::Cancel]);
    }

    #[gpui_kit::test]
    fn nothing_is_submitted_empty(cx: &mut TestAppContext) {
        let (view, seen, cx) = editor("   ", EditorOptions::REPLY, cx);
        view.update(cx, |editor, cx| editor.submit(cx));
        assert!(seen.borrow().is_empty());
    }

    #[gpui_kit::test]
    fn a_description_may_be_cleared_but_not_twice(cx: &mut TestAppContext) {
        let (view, seen, cx) = editor("", EditorOptions::DESCRIPTION, cx);
        view.update(cx, |editor, cx| editor.submit(cx));
        assert_eq!(seen.borrow().as_slice(), [Seen::Submit(String::new())]);
        view.update(cx, |editor, cx| {
            editor.set_busy(true, cx);
            editor.submit(cx);
        });
        assert_eq!(seen.borrow().len(), 1, "busy: no second submit");
    }

    #[gpui_kit::test]
    fn preview_renders_the_draft(cx: &mut TestAppContext) {
        let (view, _, cx) = editor("Fixes #12", EditorOptions::COMMENT, cx);
        view.update_in(cx, |editor, window, cx| {
            editor.set_previewing(true, window, cx)
        });
        view.read_with(cx, |editor, _| assert!(editor.preview.is_some()));
    }
}
