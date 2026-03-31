use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, Focusable, Render, Window};
use gpui_component::{
    select::{Select, SelectEvent, SelectItem, SelectState},
    ActiveTheme, Icon, Sizable, Size,
};
use std::path::PathBuf;
use tracing::debug;

/// Events emitted by the WorktreeSelector component.
#[derive(Clone, Debug)]
pub enum WorktreeSelectorEvent {
    /// User selected "Local" (no worktree) — switch back to main project dir.
    SwitchedToLocal,
    /// User selected an existing worktree.
    SwitchedToWorktree {
        worktree_path: PathBuf,
        branch: String,
    },
    /// User requested creating a new worktree.
    /// The UI should then show a branch picker or dialog.
    CreateNewWorktreeRequested,
    /// User clicked the selector, triggering a refresh of branches/worktrees.
    RefreshRequested,
}

/// An item in the worktree dropdown.
#[derive(Clone, Debug)]
struct WorktreeOption {
    label: String,
    value: WorktreeValue,
}

/// The value associated with a dropdown item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeValue {
    /// No worktree — use the main project directory.
    Local,
    /// An existing worktree.
    Worktree { path: PathBuf, branch: String },
    /// Sentinel: "New worktree..." action item.
    CreateNew,
}

impl WorktreeOption {
    fn local() -> Self {
        Self {
            label: "Local".to_string(),
            value: WorktreeValue::Local,
        }
    }

    fn worktree(path: PathBuf, branch: String) -> Self {
        let label = format!("\u{e725} {branch}"); // git branch icon (nerd font)
        Self {
            label,
            value: WorktreeValue::Worktree { path, branch },
        }
    }

    fn create_new() -> Self {
        Self {
            label: "+ New worktree...".to_string(),
            value: WorktreeValue::CreateNew,
        }
    }
}

impl SelectItem for WorktreeOption {
    type Value = WorktreeValue;

    fn title(&self) -> gpui::SharedString {
        self.label.clone().into()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        None
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

/// Dropdown component for selecting the working directory:
/// local project, an existing worktree, or creating a new one.
pub struct WorktreeSelector {
    dropdown_state: Entity<SelectState<Vec<WorktreeOption>>>,
    _subscription: gpui::Subscription,
    /// Whether the project is a git repo at all.
    is_git_repo: bool,
}

impl EventEmitter<WorktreeSelectorEvent> for WorktreeSelector {}

impl WorktreeSelector {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let items = vec![WorktreeOption::local()];
        let dropdown_state =
            cx.new(|cx| SelectState::new(Vec::<WorktreeOption>::new(), None, window, cx));

        dropdown_state.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_value(&WorktreeValue::Local, window, cx);
        });

        let subscription = cx.subscribe_in(&dropdown_state, window, Self::on_dropdown_event);

        Self {
            dropdown_state,
            _subscription: subscription,
            is_git_repo: false,
        }
    }

    /// Update the list of available worktrees from backend data.
    pub fn set_worktrees(
        &mut self,
        worktrees: &[git::Worktree],
        current_worktree_path: Option<&PathBuf>,
        is_git_repo: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_git_repo = is_git_repo;

        let mut items = vec![WorktreeOption::local()];

        if is_git_repo {
            // Add existing linked worktrees (skip the main one)
            for wt in worktrees.iter().filter(|wt| !wt.is_main) {
                if let Some(branch) = wt.branch_name() {
                    items.push(WorktreeOption::worktree(
                        wt.path.clone(),
                        branch.to_string(),
                    ));
                }
            }

            items.push(WorktreeOption::create_new());
        }

        // Determine current selection
        let selected_value = if let Some(wt_path) = current_worktree_path {
            // Find matching worktree
            worktrees
                .iter()
                .find(|wt| &wt.path == wt_path)
                .and_then(|wt| {
                    wt.branch_name().map(|b| WorktreeValue::Worktree {
                        path: wt.path.clone(),
                        branch: b.to_string(),
                    })
                })
                .unwrap_or(WorktreeValue::Local)
        } else {
            WorktreeValue::Local
        };

        self.dropdown_state.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_value(&selected_value, window, cx);
        });
    }

    /// Set the current selection to "Local".
    pub fn set_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dropdown_state.update(cx, |state, cx| {
            state.set_selected_value(&WorktreeValue::Local, window, cx);
        });
    }

    /// Set the current selection to a specific worktree.
    pub fn set_worktree(
        &mut self,
        path: PathBuf,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = WorktreeValue::Worktree { path, branch };
        self.dropdown_state.update(cx, |state, cx| {
            state.set_selected_value(&value, window, cx);
        });
    }

    fn on_dropdown_event(
        &mut self,
        _: &Entity<SelectState<Vec<WorktreeOption>>>,
        event: &SelectEvent<Vec<WorktreeOption>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(value)) = event {
            match value {
                WorktreeValue::Local => {
                    debug!("Worktree selector: switched to local");
                    cx.emit(WorktreeSelectorEvent::SwitchedToLocal);
                }
                WorktreeValue::Worktree { path, branch } => {
                    debug!("Worktree selector: switched to {:?} ({})", path, branch);
                    cx.emit(WorktreeSelectorEvent::SwitchedToWorktree {
                        worktree_path: path.clone(),
                        branch: branch.clone(),
                    });
                }
                WorktreeValue::CreateNew => {
                    debug!("Worktree selector: create new requested");
                    cx.emit(WorktreeSelectorEvent::CreateNewWorktreeRequested);
                }
            }
        }
    }
}

impl Focusable for WorktreeSelector {
    fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.dropdown_state.focus_handle(cx)
    }
}

impl Render for WorktreeSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().text_color(cx.theme().muted_foreground).child(
            Select::new(&self.dropdown_state)
                .placeholder("Working Dir")
                .with_size(Size::XSmall)
                .appearance(false)
                .icon(
                    Icon::default()
                        .path("icons/chevron_up_down.svg")
                        .with_size(Size::XSmall)
                        .text_color(cx.theme().muted_foreground),
                )
                .min_w(px(160.)),
        )
    }
}
