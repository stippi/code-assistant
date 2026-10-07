//! The form above the input area for an `ask_question` request: each
//! question with its options as radio buttons (single choice) or checkboxes
//! (`multi_select`), plus a comment field. Submitting sends one answer per
//! question; "Skip" declines. The prompt dismisses when the core reports the
//! request settled (`UserQuestionsResolved`).

use crate::Gpui;
use code_assistant_core::session::questions::{QuestionAnswer, UserQuestionRequest};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{
    AnyElement, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px,
};

pub struct QuestionPrompt {
    session_id: String,
    request: UserQuestionRequest,
    /// Per question, per option: whether it is selected.
    selected: Vec<Vec<bool>>,
    /// Per question: the comment field.
    comments: Vec<Entity<InputState>>,
    /// Set once answered, so a double click doesn't send twice.
    submitted: bool,
}

impl QuestionPrompt {
    pub fn new(
        session_id: String,
        request: UserQuestionRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected = request
            .questions
            .iter()
            .map(|q| vec![false; q.options.len()])
            .collect();
        let comments = request
            .questions
            .iter()
            .map(|_| cx.new(|cx| InputState::new(window, cx).placeholder("Comment (optional)")))
            .collect();
        Self {
            session_id,
            request,
            selected,
            comments,
            submitted: false,
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request.request_id
    }

    fn toggle(&mut self, question: usize, option: usize, cx: &mut Context<Self>) {
        let multi_select = self.request.questions[question].multi_select;
        let row = &mut self.selected[question];
        if multi_select {
            row[option] = !row[option];
        } else {
            for (i, selected) in row.iter_mut().enumerate() {
                *selected = i == option;
            }
        }
        cx.notify();
    }

    fn answers(&self, cx: &Context<Self>) -> Vec<QuestionAnswer> {
        self.request
            .questions
            .iter()
            .zip(&self.selected)
            .zip(&self.comments)
            .map(|((question, selected), comment)| QuestionAnswer {
                selected: question
                    .options
                    .iter()
                    .zip(selected)
                    .filter(|(_, selected)| **selected)
                    .map(|(option, _)| option.label.clone())
                    .collect(),
                comment: comment.read(cx).value().trim().to_string(),
            })
            .collect()
    }

    fn respond(&mut self, answers: Option<Vec<QuestionAnswer>>, cx: &mut Context<Self>) {
        if self.submitted {
            return;
        }
        self.submitted = true;
        if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.cmd_answer_questions(
                self.session_id.clone(),
                self.request.request_id.clone(),
                answers,
            );
        }
        cx.notify();
    }

    fn render_question(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let question = &self.request.questions[index];
        let rid = &self.request.request_id;

        let options = question.options.iter().enumerate().map(|(i, option)| {
            let checked = self.selected[index][i];
            let id = SharedString::from(format!("question-{rid}-{index}-{i}"));
            let control = if question.multi_select {
                Checkbox::new(id)
                    .label(option.label.clone())
                    .checked(checked)
                    .on_click(cx.listener(move |this, _: &bool, _, cx| this.toggle(index, i, cx)))
                    .into_any_element()
            } else {
                Radio::new(id)
                    .label(option.label.clone())
                    .checked(checked)
                    .on_click(cx.listener(move |this, _: &bool, _, cx| this.toggle(index, i, cx)))
                    .into_any_element()
            };
            div()
                .flex()
                .flex_col()
                .child(control)
                .when(!option.description.is_empty(), |el| {
                    el.child(
                        div()
                            .pl(px(24.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(option.description.clone()),
                    )
                })
        });

        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .when(!question.header.is_empty(), |el| {
                        el.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .rounded_sm()
                                .text_xs()
                                .bg(cx.theme().primary.opacity(0.15))
                                .text_color(cx.theme().primary)
                                .child(question.header.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .child(question.question.clone()),
                    ),
            )
            .child(div().flex().flex_col().gap_1().pl_1().children(options))
            .child(Input::new(&self.comments[index]).small())
            .into_any_element()
    }
}

impl Render for QuestionPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let questions: Vec<AnyElement> = (0..self.request.questions.len())
            .map(|index| self.render_question(index, cx))
            .collect();
        let submitted = self.submitted;

        let button = |id: &'static str, label: &'static str, primary: bool, cx: &Context<Self>| {
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded_md()
                .text_xs()
                .when(!submitted, |el| el.cursor_pointer())
                .when(submitted, |el| el.opacity(0.5))
                .when(primary, |el| {
                    el.bg(cx.theme().primary)
                        .text_color(cx.theme().primary_foreground)
                        .hover(|s| s.bg(cx.theme().primary.opacity(0.8)))
                })
                .when(!primary, |el| {
                    el.border_1()
                        .border_color(cx.theme().border)
                        .text_color(cx.theme().muted_foreground)
                        .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                })
                .child(label)
        };

        div()
            .id("question-prompt")
            .flex_none()
            .max_h(px(480.))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().primary.opacity(0.06))
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .children(questions)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("question-skip", "Skip", false, cx)
                            .debug_selector(|| "question-skip".to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.respond(None, cx))),
                    )
                    .child(
                        button("question-submit", "Submit answers", true, cx)
                            .debug_selector(|| "question-submit".to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let answers = this.answers(cx);
                                this.respond(Some(answers), cx);
                            })),
                    ),
            )
    }
}
