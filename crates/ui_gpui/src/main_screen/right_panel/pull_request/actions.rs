//! What the header offers to do with a pull request: the one prominent
//! control (merge, ready for review, auto-merge, or the state once it is
//! settled), "Check out" and the ⋯ menu. Ported from T3 Code's
//! `pullRequestDetail.logic.ts` and `PullRequestActionsMenu.tsx`.
//!
//! Merging, enabling auto-merge and closing take two clicks: the first arms
//! the control ("Confirm merge?"), the second within three seconds acts. A
//! press elsewhere disarms it.

use super::PullRequestIntent;
use super::header::DetailHeader;
use super::presentation::{Tone, checks_state, merge_method_label};
use github::model::{
    Action, CheckStatus, ChecksState, Detail, MergeMethod, Mergeability, PullRequestAction,
    PullRequestState, PullRequestUpdate, UpdateMethod,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Side, Sizable};
use gpui_kit::{
    AnyElement, App, Context, Hsla, SharedString, WeakEntity, div, prelude::*, px, svg,
};

/// The header's one prominent control, by where the pull request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimaryControl {
    /// Conflicts with the base: nothing merges until they are resolved.
    Conflicts,
    Ready,
    Merge,
    EnableAutoMerge,
    /// Auto-merge waits for the requirements.
    AutoMergeArmed,
    Merged,
    Closed,
}

/// The actions the host lets the viewer take.
pub fn can(detail: &Detail, action: PullRequestAction) -> bool {
    detail.viewer_permissions.actions.contains(&action)
}

/// Merge methods the repository allows, in GitHub's order.
pub fn allowed_merge_methods(detail: &Detail) -> Vec<MergeMethod> {
    let allows = detail.merge_capabilities;
    [
        (MergeMethod::Merge, allows.merge),
        (MergeMethod::Squash, allows.squash),
        (MergeMethod::Rebase, allows.rebase),
    ]
    .into_iter()
    .filter_map(|(method, allowed)| allowed.then_some(method))
    .collect()
}

/// The method the merge controls use: the viewer's pick while allowed,
/// else the first allowed one.
pub fn resolve_merge_method(allowed: &[MergeMethod], picked: Option<MergeMethod>) -> MergeMethod {
    picked
        .filter(|method| allowed.contains(method))
        .or_else(|| allowed.first().copied())
        .unwrap_or(MergeMethod::Merge)
}

/// The checks verdict the controls go by. A workflow waiting for approval
/// keeps a pull request pending even where the rollup forgot it.
pub fn effective_checks_state(detail: &Detail) -> Option<ChecksState> {
    let state = detail.checks_state.or_else(|| checks_state(&detail.checks));
    let awaiting = detail
        .checks
        .iter()
        .any(|check| check.status == CheckStatus::ActionRequired);
    if awaiting && state != Some(ChecksState::Failing) {
        Some(ChecksState::Pending)
    } else {
        state
    }
}

pub fn primary_control(detail: &Detail) -> Option<PrimaryControl> {
    match detail.state {
        PullRequestState::Merged => return Some(PrimaryControl::Merged),
        PullRequestState::Closed => return Some(PrimaryControl::Closed),
        PullRequestState::Open => {}
    }
    if detail.mergeability == Mergeability::Conflicting {
        return Some(PrimaryControl::Conflicts);
    }
    if detail.is_draft {
        return can(detail, PullRequestAction::Ready).then_some(PrimaryControl::Ready);
    }
    if detail.auto_merge_enabled {
        return Some(PrimaryControl::AutoMergeArmed);
    }
    if allowed_merge_methods(detail).is_empty() {
        return None;
    }
    let unsettled = effective_checks_state(detail).is_some_and(|s| s != ChecksState::Passing);
    if unsettled && detail.auto_merge_allowed && can(detail, PullRequestAction::EnableAutoMerge) {
        return Some(PrimaryControl::EnableAutoMerge);
    }
    can(detail, PullRequestAction::Merge).then_some(PrimaryControl::Merge)
}

/// Which entries the ⋯ menu shows besides the ones always there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuEntries {
    pub draft_toggle: bool,
    pub merge_now: bool,
    pub enable_auto_merge: bool,
    pub disable_auto_merge: bool,
    pub merge_methods: bool,
    pub maintainer_edits: bool,
    pub close: bool,
    pub reopen: bool,
    pub delete_branch: bool,
}

pub fn menu_entries(detail: &Detail) -> MenuEntries {
    let open = detail.state == PullRequestState::Open;
    let primary = primary_control(detail);
    let conflicting = detail.mergeability == Mergeability::Conflicting;
    let methods = allowed_merge_methods(detail).len();
    let armed = open && detail.auto_merge_enabled;
    let mergeable = open && !detail.is_draft && !conflicting && methods > 0;
    let toggle = if detail.is_draft {
        PullRequestAction::Ready
    } else {
        PullRequestAction::Draft
    };
    MenuEntries {
        draft_toggle: open
            && can(detail, toggle)
            && !(detail.is_draft && primary == Some(PrimaryControl::Ready)),
        merge_now: mergeable
            && matches!(
                primary,
                Some(PrimaryControl::EnableAutoMerge | PrimaryControl::AutoMergeArmed)
            )
            && can(detail, PullRequestAction::Merge),
        enable_auto_merge: mergeable
            && !armed
            && primary != Some(PrimaryControl::EnableAutoMerge)
            && detail.auto_merge_allowed
            && can(detail, PullRequestAction::EnableAutoMerge),
        disable_auto_merge: armed && can(detail, PullRequestAction::DisableAutoMerge),
        merge_methods: mergeable && methods > 1 && can(detail, PullRequestAction::Merge),
        maintainer_edits: open && detail.viewer_permissions.maintainer_edits,
        close: open && can(detail, PullRequestAction::Close),
        reopen: detail.state == PullRequestState::Closed && can(detail, PullRequestAction::Reopen),
        delete_branch: !open && can(detail, PullRequestAction::DeleteBranch),
    }
}

/// An open pull request whose branch fell behind its base: by how much,
/// and how the viewer may bring the base in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchUpdate {
    pub behind_by: Option<u64>,
    pub methods: Vec<UpdateMethod>,
}

pub fn branch_update(detail: &Detail) -> Option<BranchUpdate> {
    use github::model::BaseComparison;
    (detail.state == PullRequestState::Open
        && detail.base_comparison == BaseComparison::Behind
        && detail.mergeability == Mergeability::Mergeable)
        .then(|| BranchUpdate {
            behind_by: detail.behind_by,
            methods: detail.viewer_permissions.update_methods.clone(),
        })
}

/// The controls that take a second click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmable {
    Merge,
    EnableAutoMerge,
    Close,
}

/// Which control waits for its confirming click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineConfirm {
    armed: Option<Confirmable>,
}

impl InlineConfirm {
    pub fn is_armed(&self, key: Confirmable) -> bool {
        self.armed == Some(key)
    }

    /// A click on `key`: `true` when it confirms, else it arms `key`.
    pub fn click(&mut self, key: Confirmable) -> bool {
        if self.armed == Some(key) {
            self.armed = None;
            true
        } else {
            self.armed = Some(key);
            false
        }
    }

    /// Whether something was armed.
    pub fn disarm(&mut self) -> bool {
        self.armed.take().is_some()
    }
}

fn icon(path: &'static str) -> Icon {
    Icon::default().path(SharedString::from(path))
}

/// A label in a state colour with an outline, for what cannot be clicked.
fn badge(icon_path: &'static str, label: SharedString, color: Hsla) -> AnyElement {
    div()
        .flex_none()
        .h(px(24.))
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(color.opacity(0.45))
        .text_xs()
        .text_color(color)
        .child(svg().size(px(14.)).path(icon_path).text_color(color))
        .child(label)
        .into_any_element()
}

fn auto_merge_label(method: Option<MergeMethod>) -> String {
    match method {
        Some(method) => format!("Auto-merge ({})", merge_method_label(method).to_lowercase()),
        None => "Auto-merge".into(),
    }
}

/// The primary control, `None` when there is nothing to offer.
pub fn render_primary(
    header: &DetailHeader,
    detail: &Detail,
    cx: &mut Context<DetailHeader>,
) -> Option<AnyElement> {
    let method = header.merge_method(detail);
    let confirm = header.confirm();
    let control = primary_control(detail)?;
    let confirmable = |key: Confirmable, label: String, action: Action, cx: &mut Context<_>| {
        let armed = confirm.is_armed(key);
        let label = if armed {
            match key {
                Confirmable::Merge => "Confirm merge?".to_string(),
                _ => "Confirm auto-merge?".to_string(),
            }
        } else {
            label
        };
        Button::new("pr-primary")
            .primary()
            .xsmall()
            .icon(icon(if armed {
                "icons/check.svg"
            } else {
                "icons/git_merge.svg"
            }))
            .label(label)
            .on_click(cx.listener(move |this: &mut DetailHeader, _, _, cx| {
                this.click_confirmable(key, action, cx)
            }))
            .when(armed, |button| {
                button.on_mouse_down_out(
                    cx.listener(|this: &mut DetailHeader, _, _, cx| this.disarm(cx)),
                )
            })
            .into_any_element()
    };
    let element = match control {
        PrimaryControl::Merged | PrimaryControl::Closed => {
            let tone = Tone::of(detail.state, detail.is_draft);
            badge(tone.icon(), tone.label().into(), tone.color(cx))
        }
        PrimaryControl::Conflicts => badge(
            "icons/triangle_alert.svg",
            "Has conflicts".into(),
            cx.theme().danger,
        ),
        PrimaryControl::AutoMergeArmed => badge(
            "icons/git_merge.svg",
            auto_merge_label(detail.auto_merge_method).into(),
            cx.theme().info,
        ),
        PrimaryControl::Ready => Button::new("pr-primary")
            .primary()
            .xsmall()
            .icon(icon("icons/git_pull_request.svg"))
            .label("Ready for review")
            .on_click(cx.listener(|this: &mut DetailHeader, _, _, cx| {
                this.emit(PullRequestIntent::Act(Action::Ready), cx)
            }))
            .into_any_element(),
        PrimaryControl::Merge => confirmable(
            Confirmable::Merge,
            merge_method_label(method).into(),
            Action::Merge(method),
            cx,
        ),
        PrimaryControl::EnableAutoMerge => confirmable(
            Confirmable::EnableAutoMerge,
            auto_merge_label(Some(method)),
            Action::EnableAutoMerge(method),
            cx,
        ),
    };
    Some(
        div()
            .flex_none()
            .debug_selector(|| "pr-primary".into())
            .child(element)
            .into_any_element(),
    )
}

/// "Check out ▾": for now the command that checks the branch out.
pub fn render_checkout(number: u64, header: WeakEntity<DetailHeader>) -> AnyElement {
    Button::new("pr-checkout")
        .outline()
        .xsmall()
        .icon(icon("icons/git_branch.svg"))
        .label("Check out")
        .dropdown_caret(true)
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
            let header = header.clone();
            let command = checkout_command(number);
            menu.item(
                PopupMenuItem::new(format!("Copy “{command}”"))
                    .icon(icon("icons/copy.svg"))
                    .on_click(move |_, _, cx| {
                        send(&header, PullRequestIntent::Copy(command.clone()), cx)
                    }),
            )
        })
        .into_any_element()
}

pub fn checkout_command(number: u64) -> String {
    format!("gh pr checkout {number}")
}

fn send(header: &WeakEntity<DetailHeader>, intent: PullRequestIntent, cx: &mut App) {
    header.update(cx, |header, cx| header.emit(intent, cx)).ok();
}

/// A menu entry taking two clicks. The first arms it and keeps the menu
/// open (the entry's own click stops there); the second reaches the menu,
/// which acts and closes.
fn confirm_entry(
    header: &WeakEntity<DetailHeader>,
    key: Confirmable,
    labels: (&'static str, &'static str),
    icon_path: &'static str,
    action: Action,
) -> PopupMenuItem {
    let render_header = header.clone();
    let click_header = header.clone();
    PopupMenuItem::element(move |_, cx| {
        let armed = render_header
            .upgrade()
            .is_some_and(|h| h.read(cx).confirm().is_armed(key));
        let arm_header = render_header.clone();
        div()
            .id(SharedString::from(format!("pr-confirm-{key:?}")))
            .flex_1()
            .when(armed, |el| el.text_color(cx.theme().danger))
            .child(if armed { labels.1 } else { labels.0 })
            .when(!armed, |el| {
                el.on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    arm_header
                        .update(cx, |h, cx| {
                            h.click_confirmable(key, action, cx);
                        })
                        .ok();
                })
            })
    })
    .icon(icon(icon_path))
    .on_click(move |_, _, cx| {
        click_header
            .update(cx, |h, cx| h.click_confirmable(key, action, cx))
            .ok();
    })
}

/// The ⋯ menu.
pub fn render_more_menu(detail: &Detail, header: WeakEntity<DetailHeader>) -> AnyElement {
    let detail = detail.clone();
    let closing = header.clone();
    Button::new("pr-more")
        .ghost()
        .xsmall()
        .icon(icon("icons/ellipsis.svg"))
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, cx| {
            more_menu(menu, &detail, &header, cx)
        })
        .on_open_change(move |open, _, cx| {
            if !open {
                closing.update(cx, |h, cx| h.disarm(cx)).ok();
            }
        })
        .into_any_element()
}

fn more_menu(
    menu: PopupMenu,
    detail: &Detail,
    header: &WeakEntity<DetailHeader>,
    cx: &App,
) -> PopupMenu {
    let entries = menu_entries(detail);
    let method = header
        .upgrade()
        .map_or(MergeMethod::Merge, |h| h.read(cx).merge_method(detail));
    let item = |label: &str, icon_path: &'static str, intent: PullRequestIntent| {
        let header = header.clone();
        PopupMenuItem::new(SharedString::from(label.to_owned()))
            .icon(icon(icon_path))
            .on_click(move |_, _, cx| send(&header, intent.clone(), cx))
    };
    let mut menu = menu
        .min_w(px(220.))
        .check_side(Side::Right)
        .item(item(
            "Refresh",
            "icons/rotate_cw.svg",
            PullRequestIntent::Refresh,
        ))
        .separator();
    if entries.maintainer_edits {
        let allowed = detail.maintainer_can_modify;
        let update = PullRequestUpdate {
            maintainer_can_modify: Some(!allowed),
            ..PullRequestUpdate::default()
        };
        menu = menu
            .item(
                item(
                    "Allow edits from maintainers",
                    "icons/pen_line.svg",
                    PullRequestIntent::Update(update),
                )
                .checked(allowed),
            )
            .separator();
    }
    if entries.draft_toggle {
        menu = menu.item(if detail.is_draft {
            item(
                "Ready for review",
                "icons/git_pull_request.svg",
                PullRequestIntent::Act(Action::Ready),
            )
        } else {
            item(
                "Convert to draft",
                "icons/git_pull_request_draft.svg",
                PullRequestIntent::Act(Action::Draft),
            )
        });
    }
    if entries.merge_now {
        menu = menu.item(confirm_entry(
            header,
            Confirmable::Merge,
            ("Merge now", "Confirm merge?"),
            "icons/git_merge.svg",
            Action::Merge(method),
        ));
    }
    if entries.disable_auto_merge {
        menu = menu.item(item(
            "Disable auto-merge",
            "icons/git_merge.svg",
            PullRequestIntent::Act(Action::DisableAutoMerge),
        ));
    } else if entries.enable_auto_merge {
        menu = menu.item(confirm_entry(
            header,
            Confirmable::EnableAutoMerge,
            ("Enable auto-merge", "Confirm auto-merge?"),
            "icons/git_merge.svg",
            Action::EnableAutoMerge(method),
        ));
    }
    if entries.merge_methods {
        menu = menu.separator();
        for allowed in allowed_merge_methods(detail) {
            let header = header.clone();
            menu = menu.item(
                PopupMenuItem::new(merge_method_label(allowed))
                    .icon(icon("icons/git_merge.svg"))
                    .checked(allowed == method)
                    .on_click(move |_, _, cx| {
                        header
                            .update(cx, |h, cx| h.pick_merge_method(allowed, cx))
                            .ok();
                    }),
            );
        }
    }
    menu = menu
        .separator()
        .item(item(
            "Open on GitHub",
            "icons/external_link.svg",
            PullRequestIntent::OpenUrl(detail.url.clone()),
        ))
        .item(item(
            "Copy link",
            "icons/link.svg",
            PullRequestIntent::Copy(detail.url.clone()),
        ))
        .item(item(
            "Copy PR number",
            "icons/copy.svg",
            PullRequestIntent::Copy(format!("#{}", detail.number)),
        ));
    if entries.close {
        menu = menu.separator().item(confirm_entry(
            header,
            Confirmable::Close,
            ("Close pull request", "Confirm close?"),
            "icons/git_pull_request_closed.svg",
            Action::Close,
        ));
    } else if entries.reopen {
        menu = menu.separator().item(item(
            "Reopen pull request",
            "icons/git_pull_request.svg",
            PullRequestIntent::Act(Action::Reopen),
        ));
    }
    if entries.delete_branch {
        menu = menu.separator().item(item(
            &format!("Delete branch {}", detail.head_branch),
            "icons/trash.svg",
            PullRequestIntent::Act(Action::DeleteBranch),
        ));
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{check, detail, detail_with};
    use github::model::{BaseComparison, MergeCapabilities};

    #[test]
    fn settled_pull_requests_show_their_state() {
        let merged = detail_with(1, |d| d.state = PullRequestState::Merged);
        assert_eq!(primary_control(&merged), Some(PrimaryControl::Merged));
        let closed = detail_with(1, |d| d.state = PullRequestState::Closed);
        assert_eq!(primary_control(&closed), Some(PrimaryControl::Closed));
    }

    #[test]
    fn conflicts_come_before_everything_open() {
        let conflicting = detail_with(1, |d| {
            d.mergeability = Mergeability::Conflicting;
            d.is_draft = true;
        });
        assert_eq!(
            primary_control(&conflicting),
            Some(PrimaryControl::Conflicts)
        );
    }

    #[test]
    fn a_draft_offers_ready_for_review_when_allowed() {
        let draft = detail_with(1, |d| d.is_draft = true);
        assert_eq!(primary_control(&draft), Some(PrimaryControl::Ready));
        let draft = detail_with(1, |d| {
            d.is_draft = true;
            d.viewer_permissions.actions.clear();
        });
        assert_eq!(primary_control(&draft), None);
    }

    #[test]
    fn pending_checks_offer_auto_merge_passing_ones_merge() {
        let pending = detail_with(1, |d| d.checks = vec![check("ci", CheckStatus::Pending)]);
        assert_eq!(
            primary_control(&pending),
            Some(PrimaryControl::EnableAutoMerge)
        );
        let passing = detail_with(1, |d| d.checks = vec![check("ci", CheckStatus::Success)]);
        assert_eq!(primary_control(&passing), Some(PrimaryControl::Merge));
        // Without checks there is nothing to wait for.
        assert_eq!(primary_control(&detail(1)), Some(PrimaryControl::Merge));
        let no_auto_merge = detail_with(1, |d| {
            d.checks_state = Some(ChecksState::Failing);
            d.auto_merge_allowed = false;
        });
        assert_eq!(primary_control(&no_auto_merge), Some(PrimaryControl::Merge));
        let armed = detail_with(1, |d| d.auto_merge_enabled = true);
        assert_eq!(
            primary_control(&armed),
            Some(PrimaryControl::AutoMergeArmed)
        );
    }

    #[test]
    fn a_workflow_awaiting_approval_keeps_checks_pending() {
        let awaiting = detail_with(1, |d| {
            d.checks_state = Some(ChecksState::Passing);
            d.checks = vec![check("build", CheckStatus::ActionRequired)];
        });
        assert_eq!(
            effective_checks_state(&awaiting),
            Some(ChecksState::Pending)
        );
    }

    #[test]
    fn nothing_to_merge_with_offers_nothing() {
        let no_methods = detail_with(1, |d| d.merge_capabilities = MergeCapabilities::default());
        assert_eq!(primary_control(&no_methods), None);
        let no_permission = detail_with(1, |d| d.viewer_permissions.actions.clear());
        assert_eq!(primary_control(&no_permission), None);
    }

    #[test]
    fn the_merge_method_is_the_pick_while_allowed() {
        let all = [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase];
        assert_eq!(resolve_merge_method(&all, None), MergeMethod::Merge);
        assert_eq!(
            resolve_merge_method(&all, Some(MergeMethod::Rebase)),
            MergeMethod::Rebase
        );
        assert_eq!(
            resolve_merge_method(&[MergeMethod::Squash], Some(MergeMethod::Rebase)),
            MergeMethod::Squash
        );
        let squash_only = detail_with(1, |d| {
            d.merge_capabilities = MergeCapabilities {
                merge: false,
                squash: true,
                rebase: false,
            }
        });
        assert_eq!(
            allowed_merge_methods(&squash_only),
            vec![MergeMethod::Squash]
        );
    }

    #[test]
    fn menu_entries_follow_state_and_permissions() {
        let open = menu_entries(&detail(1));
        assert!(open.draft_toggle && open.enable_auto_merge && open.merge_methods && open.close);
        assert!(!open.merge_now && !open.reopen && !open.delete_branch);

        let pending = menu_entries(&detail_with(1, |d| {
            d.checks_state = Some(ChecksState::Pending)
        }));
        // Auto-merge is the primary control, so merging at once moves here.
        assert!(pending.merge_now && !pending.enable_auto_merge);

        let armed = menu_entries(&detail_with(1, |d| d.auto_merge_enabled = true));
        assert!(armed.disable_auto_merge && armed.merge_now && !armed.enable_auto_merge);

        let draft = menu_entries(&detail_with(1, |d| d.is_draft = true));
        // Ready for review is the primary control already.
        assert!(!draft.draft_toggle && !draft.merge_methods);

        let closed = menu_entries(&detail_with(1, |d| d.state = PullRequestState::Closed));
        assert!(closed.reopen && closed.delete_branch && !closed.close);
    }

    #[test]
    fn a_branch_behind_its_base_offers_an_update() {
        assert_eq!(branch_update(&detail(1)), None);
        let behind = detail_with(1, |d| {
            d.base_comparison = BaseComparison::Behind;
            d.behind_by = Some(3);
            d.viewer_permissions.update_methods = vec![UpdateMethod::Merge];
        });
        assert_eq!(
            branch_update(&behind),
            Some(BranchUpdate {
                behind_by: Some(3),
                methods: vec![UpdateMethod::Merge],
            })
        );
    }

    #[test]
    fn the_second_click_confirms() {
        let mut confirm = InlineConfirm::default();
        assert!(!confirm.click(Confirmable::Merge));
        assert!(confirm.is_armed(Confirmable::Merge));
        // Another control re-arms instead of confirming.
        assert!(!confirm.click(Confirmable::Close));
        assert!(!confirm.is_armed(Confirmable::Merge));
        assert!(confirm.click(Confirmable::Close));
        assert!(!confirm.disarm());
    }
}
