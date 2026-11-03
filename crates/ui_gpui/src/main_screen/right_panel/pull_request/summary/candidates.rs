//! The Reviewers and Labels pickers wired to GitHub: opened from their
//! icon button, they read the candidates, and each toggle requests a
//! reviewer or applies a label (ticked once the write went through).

use super::SummaryTab;
use super::meta::label_color;
use super::picker::{
    Candidate, CandidatePicker, LABEL_TEXTS, Leading, PickerEntry, PickerToggle, REVIEWER_TEXTS,
};
use crate::main_screen::right_panel::pull_request::data::PullRequestData;
use github::model::{Actor, LabelCandidates, Reviewer, ReviewerCandidates, ReviewerKind};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{
    AnyElement, Context, Entity, SharedString, Subscription, Task, WeakEntity, Window, div,
    prelude::*,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PickerKind {
    Reviewers,
    Labels,
}

pub(super) struct OpenPicker {
    pub(super) picker: Entity<CandidatePicker>,
    _toggle: Subscription,
    _read: Task<()>,
}

impl SummaryTab {
    fn picker_slot(&mut self, kind: PickerKind) -> &mut Option<OpenPicker> {
        match kind {
            PickerKind::Reviewers => &mut self.reviewer_picker,
            PickerKind::Labels => &mut self.label_picker,
        }
    }

    pub(super) fn open_picker(
        &mut self,
        kind: PickerKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let texts = match kind {
            PickerKind::Reviewers => REVIEWER_TEXTS,
            PickerKind::Labels => LABEL_TEXTS,
        };
        let picker = cx.new(|cx| CandidatePicker::new(texts, window, cx));
        let toggle = cx.subscribe(&picker, |this, picker, toggle: &PickerToggle, cx| {
            this.toggle_candidate(picker, toggle.0.clone(), toggle.1, cx)
        });
        let read = match kind {
            PickerKind::Reviewers => self.read_reviewer_candidates(picker.downgrade(), cx),
            PickerKind::Labels => self.read_label_candidates(picker.downgrade(), cx),
        };
        *self.picker_slot(kind) = Some(OpenPicker {
            picker,
            _toggle: toggle,
            _read: read,
        });
        cx.notify();
    }

    pub(super) fn close_picker(&mut self, kind: PickerKind, cx: &mut Context<Self>) {
        if self.picker_slot(kind).take().is_some() {
            cx.notify();
        }
    }

    fn read_reviewer_candidates(
        &self,
        picker: WeakEntity<CandidatePicker>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let read = PullRequestData::write(&self.data, cx, |service, pr| async move {
            service.pull_request_reviewer_candidates(pr).await
        });
        cx.spawn(async move |_, cx| {
            let read: anyhow::Result<ReviewerCandidates> = read.await;
            picker
                .update(cx, |picker, cx| match read {
                    Ok(read) => picker.set_entries(
                        read.candidates
                            .into_iter()
                            .map(|c| PickerEntry {
                                label: c.login.clone().into(),
                                detail: match c.kind {
                                    ReviewerKind::Team => Some("team".into()),
                                    ReviewerKind::User => c.name.clone().map(Into::into),
                                },
                                leading: Leading::Avatar(Actor {
                                    login: c.login.clone(),
                                    name: c.name.clone(),
                                    avatar_url: c.avatar_url.clone(),
                                    is_bot: false,
                                }),
                                checked: c.is_requested,
                                candidate: Candidate::Reviewer(Reviewer {
                                    id: c.id,
                                    kind: c.kind,
                                }),
                            })
                            .collect(),
                        read.truncated,
                        cx,
                    ),
                    Err(_) => picker.set_failed(cx),
                })
                .ok();
        })
    }

    fn read_label_candidates(
        &self,
        picker: WeakEntity<CandidatePicker>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let read = PullRequestData::write(&self.data, cx, |service, pr| async move {
            service.pull_request_label_candidates(pr).await
        });
        cx.spawn(async move |_, cx| {
            let read: anyhow::Result<LabelCandidates> = read.await;
            picker
                .update(cx, |picker, cx| match read {
                    Ok(read) => picker.set_entries(
                        read.candidates
                            .into_iter()
                            .map(|c| PickerEntry {
                                label: c.name.clone().into(),
                                detail: c.description.clone().map(Into::into),
                                leading: Leading::Dot(label_color(c.color.as_deref())),
                                checked: c.is_applied,
                                candidate: Candidate::Label(c.name),
                            })
                            .collect(),
                        read.truncated,
                        cx,
                    ),
                    Err(_) => picker.set_failed(cx),
                })
                .ok();
        })
    }

    fn toggle_candidate(
        &mut self,
        picker: Entity<CandidatePicker>,
        candidate: Candidate,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        picker.update(cx, |picker, cx| picker.set_busy(true, cx));
        let write = match candidate.clone() {
            Candidate::Reviewer(reviewer) => {
                PullRequestData::write(&self.data, cx, move |service, pr| async move {
                    service
                        .pull_request_request_reviewers(pr, vec![reviewer], on)
                        .await
                })
            }
            Candidate::Label(name) => {
                PullRequestData::write(&self.data, cx, move |service, pr| async move {
                    service.pull_request_set_labels(pr, vec![name], on).await
                })
            }
        };
        cx.spawn(async move |_, cx| {
            let done = write.await.is_ok();
            picker.update(cx, |picker, cx| {
                picker.set_busy(false, cx);
                if done {
                    picker.set_checked(&candidate, on, cx);
                }
            });
        })
        .detach();
    }

    /// The icon button opening a picker.
    pub(super) fn picker_trigger(&self, kind: PickerKind, cx: &mut Context<Self>) -> AnyElement {
        let (id, icon, tooltip, slot) = match kind {
            PickerKind::Reviewers => (
                "pr-reviewers-picker",
                "icons/user_plus.svg",
                "Request a review",
                &self.reviewer_picker,
            ),
            PickerKind::Labels => (
                "pr-labels-picker",
                "icons/tag.svg",
                "Change labels",
                &self.label_picker,
            ),
        };
        let picker = slot.as_ref().map(|open| open.picker.clone());
        let this = cx.entity().downgrade();
        let muted = cx.theme().muted_foreground;
        Popover::new(id)
            .anchor(gpui_kit::Anchor::TopLeft)
            .open(picker.is_some())
            .on_open_change(move |open, window, cx| {
                let open = *open;
                this.update(cx, |this, cx| {
                    if open {
                        this.open_picker(kind, window, cx)
                    } else {
                        this.close_picker(kind, cx)
                    }
                })
                .ok();
            })
            .trigger(
                Button::new(SharedString::from(format!("{id}-trigger")))
                    .ghost()
                    .xsmall()
                    .icon(Icon::default().path(icon).text_color(muted))
                    .tooltip(tooltip),
            )
            .content(move |_, _, _| match &picker {
                Some(picker) => picker.clone().into_any_element(),
                None => div().into_any_element(),
            })
            .into_any_element()
    }
}
