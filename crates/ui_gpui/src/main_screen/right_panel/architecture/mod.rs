//! The "Architecture" view of the right panel: the session's project as
//! boxes of components, one level at a time, with arrows from each
//! component to the ones it uses.
//!
//! The structure comes from a deterministic scan in the core
//! (`SessionService::architecture_overview`); what each component is for,
//! its constraints and its key interfaces come from one model call per
//! component (`SessionService::architecture_summary`), cached by the
//! component's digest. Clicking a box opens its level; clicking a file
//! opens it in the Files view. Hovering a box shows its details below the
//! graph and highlights its dependencies. A [`TreeWatcher`] rescans when
//! source files or manifests change, and a changed component is explained
//! again — its previous explanation stays visible meanwhile.
//!
//! Components take comments like file lines and chat passages do: the
//! comment button on a box (or on the shown component's card) opens the
//! [`CommentEditor`]; the comment goes to the composer's draft with what the
//! overview said about the component, and commented boxes are marked.

mod layout;

use crate::Gpui;
use crate::comments::CommentChange;
use crate::comments::editor::{CommentEditor, CommentEditorEvent};
use crate::shared::file_icons;
use code_assistant_core::architecture::{
    ArchitectureConfig, ArchitectureModel, Component, ComponentKind, ComponentSummary,
    is_relevant_path,
};
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::session::TreeWatcher;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    App, Bounds, Context, Entity, FocusHandle, Focusable, Hsla, PathBuilder, Pixels, Point, Render,
    SharedString, Subscription, Task, Window, canvas, div, point, prelude::*, px,
};
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Quiet period before the watcher reports a burst of changes.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(1500);
const PAD: f32 = 12.;
const MIN_BOX_WIDTH: f32 = 150.;
const MAX_BOX_WIDTH: f32 = 230.;
const BOX_HEIGHT: f32 = 76.;
const GAP_X: f32 = 18.;
const GAP_Y: f32 = 34.;
/// Room for the label above the components without dependencies.
const ISOLATED_GAP: f32 = 22.;
/// Boxes shown per level; a larger folder lists the rest by name.
const MAX_BOXES: usize = 60;

/// Asks the right panel to show a file in the Files view.
#[derive(Clone, Debug)]
pub struct OpenFileInFiles(pub String);

impl gpui_kit::EventEmitter<OpenFileInFiles> for ArchitectureView {}

/// Comments made on components, for the composer's draft.
impl gpui_kit::EventEmitter<CommentChange> for ArchitectureView {}

pub struct ArchitectureView {
    session_id: Option<String>,
    root: Option<PathBuf>,
    model: Option<Arc<ArchitectureModel>>,
    scanning: bool,
    scan_error: Option<String>,
    /// Another scan was asked for while one ran.
    rescan_queued: bool,
    scan_task: Option<Task<()>>,
    /// Bumped by every finished scan, so a summary asked before it can
    /// tell it may be outdated.
    generation: u64,
    /// The component whose parts are shown.
    current: String,
    /// The shown component per session, while the view lives.
    levels: HashMap<String, String>,
    /// Best known explanation per component (possibly stale).
    summaries: HashMap<String, ComponentSummary>,
    /// Components whose explanation matches their current digest.
    fresh: HashSet<String>,
    summary_tasks: HashMap<String, Task<()>>,
    summary_errors: HashMap<String, String>,
    config: ArchitectureConfig,
    /// The box under the pointer, and the last one (for the inspector).
    hovered: Option<String>,
    inspected: Option<String>,
    /// The shown component's card shows responsibility, interfaces and
    /// relations too, not only its summary and constraints.
    details: bool,
    /// Graph width as last painted; the layout follows it.
    graph_width: Rc<Cell<Option<Pixels>>>,
    /// The draft's comments (the ones on components are shown here).
    comments: Vec<LineComment>,
    /// The open comment card and where it floats (window coordinates).
    comment_editor: Option<(Entity<CommentEditor>, Point<Pixels>)>,
    _comment_editor_subscription: Option<Subscription>,
    /// A comment to open once the editor can be placed (needs a window).
    comment_request: Option<LineComment>,
    /// A comment to reveal once the scan named the project root.
    pending_reveal: Option<LineComment>,
    watcher: Option<(PathBuf, TreeWatcher)>,
    watch_task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

impl ArchitectureView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            session_id: None,
            root: None,
            model: None,
            scanning: false,
            scan_error: None,
            rescan_queued: false,
            scan_task: None,
            generation: 0,
            current: String::new(),
            levels: HashMap::new(),
            summaries: HashMap::new(),
            fresh: HashSet::new(),
            summary_tasks: HashMap::new(),
            summary_errors: HashMap::new(),
            config: ArchitectureConfig::load(),
            hovered: None,
            inspected: None,
            details: false,
            graph_width: Rc::new(Cell::new(None)),
            comments: Vec::new(),
            comment_editor: None,
            _comment_editor_subscription: None,
            comment_request: None,
            pending_reveal: None,
            watcher: None,
            watch_task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Show `session_id`'s project; `None` detaches the view (no watcher).
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        if self.session_id == session_id {
            return;
        }
        if let Some(old) = &self.session_id {
            self.levels.insert(old.clone(), self.current.clone());
        }
        self.session_id = session_id.clone();
        self.root = None;
        self.model = None;
        self.scanning = false;
        self.scan_error = None;
        self.rescan_queued = false;
        self.scan_task = None;
        self.summaries.clear();
        self.fresh.clear();
        self.summary_tasks.clear();
        self.summary_errors.clear();
        self.hovered = None;
        self.inspected = None;
        self.comment_editor = None;
        self._comment_editor_subscription = None;
        self.comment_request = None;
        self.watcher = None;
        self.watch_task = None;
        self.current = session_id
            .as_ref()
            .and_then(|id| self.levels.get(id).cloned())
            .unwrap_or_default();
        if session_id.is_some() {
            self.rescan(cx);
        }
        cx.notify();
    }

    /// Scan again and explain the shown component if it changed.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.rescan(cx);
    }

    /// The draft's comments, for the markers.
    pub fn set_comments(&mut self, comments: Vec<LineComment>, cx: &mut Context<Self>) {
        self.comments = comments;
        cx.notify();
    }

    /// The absolute path a comment on component `id` refers to.
    fn component_path(&self, id: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        Some(if id.is_empty() {
            root.clone()
        } else {
            root.join(id)
        })
    }

    /// The component a comment is on, if it is one of this project's.
    fn comment_component(&self, comment: &LineComment) -> Option<String> {
        if !comment.on_component {
            return None;
        }
        let rel = comment.file.strip_prefix(self.root.as_ref()?).ok()?;
        let id = rel
            .iter()
            .map(|c| c.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        self.model.as_ref()?.get(&id).map(|_| id)
    }

    /// The draft's comment on component `id`, if any.
    fn comment_on(&self, id: &str) -> Option<&LineComment> {
        let path = self.component_path(id)?;
        self.comments
            .iter()
            .find(|c| c.on_component && c.file == path)
    }

    /// What the overview says about component `id`, sent along with a
    /// comment on it so the model knows what the user looked at.
    fn component_excerpt(&self, model: &ArchitectureModel, id: &str) -> String {
        let Some(component) = model.get(id) else {
            return String::new();
        };
        let path = if id.is_empty() { "." } else { id };
        let mut lines = vec![format!(
            "{} ({}) at {path}",
            component.name,
            component.kind.label()
        )];
        let own = self.summaries.get(id);
        if let Some(line) = own
            .map(|s| s.summary.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| self.part_line(model, id))
        {
            lines.push(line);
        }
        if let Some(own) = own {
            lines.extend(own.constraints.iter().map(|c| format!("Constraint: {c}")));
        }
        let uses = names(model, model.uses(id));
        if !uses.is_empty() {
            lines.push(format!("Uses: {}", uses.join(", ")));
        }
        let used_by = names(model, model.used_by(id));
        if !used_by.is_empty() {
            lines.push(format!("Used by: {}", used_by.join(", ")));
        }
        let parts: Vec<&str> = component
            .children
            .iter()
            .take(30)
            .map(|c| model.name(c))
            .collect();
        if !parts.is_empty() {
            lines.push(format!("Parts: {}", parts.join(", ")));
        }
        lines.join("\n")
    }

    /// Open the comment card for component `id` (its existing comment, or a
    /// new one) at `position`.
    fn start_comment(
        &mut self,
        id: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let comment = match self.comment_on(id) {
            Some(existing) => existing.clone(),
            None => {
                let (Some(model), Some(path)) = (self.model.clone(), self.component_path(id))
                else {
                    return;
                };
                LineComment::on_component(path, self.component_excerpt(&model, id))
            }
        };
        self.open_comment_editor(comment, position, window, cx);
    }

    fn open_comment_editor(
        &mut self,
        comment: LineComment,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| CommentEditor::new(comment, window, cx));
        self._comment_editor_subscription = Some(cx.subscribe(
            &editor,
            |this, _, event: &CommentEditorEvent, cx| {
                match event {
                    CommentEditorEvent::Save(comment) => {
                        cx.emit(CommentChange::Upsert(comment.clone()))
                    }
                    CommentEditorEvent::Delete(id) => cx.emit(CommentChange::Remove(*id)),
                    CommentEditorEvent::Cancel => {}
                }
                this.comment_editor = None;
                this._comment_editor_subscription = None;
                cx.notify();
            },
        ));
        self.comment_editor = Some((editor, position));
        cx.notify();
    }

    /// Show the component `comment` is on and open it for editing.
    pub fn reveal_comment(&mut self, comment: LineComment, cx: &mut Context<Self>) {
        if self.model.is_none() {
            self.pending_reveal = Some(comment);
            return;
        }
        let Some(id) = self.comment_component(&comment) else {
            return;
        };
        let parent = self
            .model
            .as_ref()
            .and_then(|m| m.get(&id))
            .and_then(|c| c.parent.clone());
        // The component as a box on its parent's level, the project as the
        // shown level.
        let level = parent.unwrap_or_default();
        if level != self.current {
            self.navigate(level, cx);
        }
        if !id.is_empty() {
            self.inspected = Some(id);
        }
        self.comment_request = Some(comment);
        cx.notify();
    }

    fn service(cx: &App) -> Option<code_assistant_core::session::SessionService> {
        cx.try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        if self.scanning {
            self.rescan_queued = true;
            return;
        }
        let Some(service) = Self::service(cx) else {
            return;
        };
        self.config = ArchitectureConfig::load();
        self.scanning = true;
        cx.notify();
        self.scan_task = Some(cx.spawn(async move |this, cx| {
            let result = service.architecture_overview(session_id.clone()).await;
            this.update(cx, |this, cx| {
                if this.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                this.scanning = false;
                this.generation += 1;
                match result {
                    Ok(overview) => {
                        this.scan_error = None;
                        for (id, cached) in overview.summaries {
                            if cached.fresh {
                                this.fresh.insert(id.clone());
                            } else {
                                this.fresh.remove(&id);
                            }
                            this.summaries.insert(id, cached.summary);
                        }
                        let model = overview.model;
                        if model.get(&this.current).is_none() {
                            this.current = nearest_existing(&model, &this.current);
                        }
                        let root = model.root.clone();
                        this.model = Some(model);
                        if this.root.as_ref() != Some(&root) {
                            this.root = Some(root.clone());
                            this.start_watcher(root, cx);
                        }
                        if let Some(comment) = this.pending_reveal.take() {
                            this.reveal_comment(comment, cx);
                        }
                        if this.config.auto_summarize {
                            this.explain(this.current.clone(), false, cx);
                        }
                    }
                    Err(e) => this.scan_error = Some(format!("{e:#}")),
                }
                if std::mem::take(&mut this.rescan_queued) {
                    this.rescan(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Ask for the explanation of `id` unless a current one is known.
    fn explain(&mut self, id: String, force: bool, cx: &mut Context<Self>) {
        if self.summary_tasks.contains_key(&id) || (!force && self.fresh.contains(&id)) {
            return;
        }
        let (Some(session_id), Some(service)) = (self.session_id.clone(), Self::service(cx)) else {
            return;
        };
        let generation = self.generation;
        self.summary_errors.remove(&id);
        let key = id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = service
                .architecture_summary(session_id.clone(), id.clone(), force)
                .await;
            this.update(cx, |this, cx| {
                if this.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                this.summary_tasks.remove(&id);
                match result {
                    Ok(summary) => {
                        this.summaries.insert(id.clone(), summary);
                        this.fresh.insert(id.clone());
                        // A scan finished meanwhile: the digest may have
                        // moved on (asking again is a cache hit if not).
                        if this.generation != generation && id == this.current {
                            this.fresh.remove(&id);
                            this.explain(id, false, cx);
                        }
                    }
                    Err(e) => {
                        this.summary_errors.insert(id, format!("{e:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.summary_tasks.insert(key, task);
        cx.notify();
    }

    fn start_watcher(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.watcher = None;
        self.watch_task = None;
        if !self.config.watch {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<BTreeSet<PathBuf>>();
        match TreeWatcher::start(&root, WATCH_DEBOUNCE, move |changed| {
            let _ = tx.send_blocking(changed);
        }) {
            Ok(watcher) => {
                self.watcher = Some((root, watcher));
                self.watch_task = Some(cx.spawn(async move |this, cx| {
                    while let Ok(changed) = rx.recv().await {
                        let relevant = changed.iter().any(|p| {
                            let rel = p
                                .iter()
                                .map(|c| c.to_string_lossy())
                                .collect::<Vec<_>>()
                                .join("/");
                            is_relevant_path(&rel)
                        });
                        if relevant && this.update(cx, |this, cx| this.rescan(cx)).is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(e) => tracing::warn!("Architecture view: watcher unavailable: {e:#}"),
        }
    }

    fn navigate(&mut self, id: String, cx: &mut Context<Self>) {
        self.current = id.clone();
        self.hovered = None;
        self.inspected = None;
        if self.config.auto_summarize {
            self.explain(id, false, cx);
        }
        cx.notify();
    }

    fn on_box_clicked(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(component) = self.model.as_ref().and_then(|m| m.get(&id)) else {
            return;
        };
        if component.kind == ComponentKind::File {
            cx.emit(OpenFileInFiles(id));
        } else {
            self.navigate(id, cx);
        }
    }

    fn set_hovered(&mut self, id: &str, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.hovered = Some(id.to_owned());
            self.inspected = Some(id.to_owned());
        } else if self.hovered.as_deref() == Some(id) {
            self.hovered = None;
        }
        cx.notify();
    }

    /// The one-line responsibility of child `id` of the shown component.
    fn part_line(&self, model: &ArchitectureModel, id: &str) -> Option<String> {
        let component = model.get(id)?;
        if let Some(line) = self
            .summaries
            .get(&self.current)
            .and_then(|s| s.parts.get(&component.name))
        {
            return Some(line.clone());
        }
        if let Some(summary) = self.summaries.get(id).filter(|s| !s.summary.is_empty()) {
            return Some(summary.summary.clone());
        }
        component.description.as_deref().map(first_sentence)
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let crumbs: Vec<gpui_kit::AnyElement> = self
            .model
            .as_ref()
            .map(|model| {
                let chain = model.ancestry(&self.current);
                let last = chain.len().saturating_sub(1);
                chain
                    .into_iter()
                    .enumerate()
                    .flat_map(|(ix, id)| {
                        let name = model.name(&id).to_owned();
                        let current = ix == last;
                        let separator = (ix > 0).then(|| {
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("›")
                                .into_any_element()
                        });
                        let selector = format!("architecture-crumb-{name}");
                        let crumb = div()
                            .id(("architecture-crumb", ix))
                            .debug_selector(move || selector.clone())
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .px_1()
                            .rounded_sm()
                            .text_color(if current {
                                theme.foreground
                            } else {
                                theme.muted_foreground
                            })
                            .when(current, |d| d.font_weight(gpui_kit::FontWeight::SEMIBOLD))
                            .when(!current, |d| {
                                d.cursor_pointer().hover(|s| s.bg(theme.list_hover))
                            })
                            .child(name)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !current {
                                    this.navigate(id.clone(), cx);
                                }
                            }))
                            .into_any_element();
                        separator.into_iter().chain(std::iter::once(crumb))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let busy = if self.scanning {
            Some("Scanning…")
        } else if !self.summary_tasks.is_empty() {
            Some("Explaining…")
        } else {
            None
        };
        let current = self.current.clone();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .overflow_hidden()
                    .children(crumbs),
            )
            .children(busy.map(|text| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(text)
            }))
            .child(
                Button::new("architecture-refresh")
                    .icon(
                        Icon::default()
                            .path(SharedString::from("icons/arrow_circle.svg"))
                            .with_size(Size::XSmall),
                    )
                    .ghost()
                    .xsmall()
                    .tooltip("Rescan and explain this level again")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.rescan(cx);
                        this.explain(current.clone(), true, cx);
                    })),
            )
    }

    /// What the shown component is, does and must respect.
    fn render_about(
        &self,
        model: &ArchitectureModel,
        component: &Component,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let summary = self.summaries.get(&component.id);
        let stale = summary.is_some() && !self.fresh.contains(&component.id);
        let explaining = self.summary_tasks.contains_key(&component.id);
        let error = self.summary_errors.get(&component.id);

        let mut card = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(kind_icon(component, theme.muted_foreground))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(component.name.clone()),
                    )
                    .child(badge(component.kind.label(), cx))
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(size_label(component)),
                    )
                    .child(comment_button(
                        "architecture-comment-current",
                        "architecture-comment-current".to_owned(),
                        component.id.clone(),
                        self.comment_on(&component.id).is_some(),
                        cx,
                    )),
            );
        if let Some(comment) = self.comment_on(&component.id) {
            card = card.child(comment_note(comment.text.clone(), cx));
        }

        match summary {
            Some(summary) => {
                if !summary.summary.is_empty() {
                    card = card.child(
                        div()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child(summary.summary.clone()),
                    );
                }
                if self.details && !summary.responsibility.is_empty() {
                    card = card.child(
                        div()
                            .text_xs()
                            .text_color(theme.foreground)
                            .child(summary.responsibility.clone()),
                    );
                }
                if !summary.constraints.is_empty() {
                    card = card.child(section_title("Constraints", cx)).children(
                        summary
                            .constraints
                            .iter()
                            .map(|c| bullet(c.clone(), theme.foreground, cx)),
                    );
                }
                if self.details && !summary.interfaces.is_empty() {
                    card = card.child(section_title("Key interfaces", cx)).children(
                        summary
                            .interfaces
                            .iter()
                            .map(|i| interface_line(i.name.clone(), i.description.clone(), cx)),
                    );
                }
            }
            None => {
                if let Some(description) = &component.description {
                    card = card.child(
                        div()
                            .text_xs()
                            .text_color(theme.foreground)
                            .child(description.clone()),
                    );
                }
            }
        }

        if self.details {
            let uses = names(model, model.uses(&component.id));
            let used_by = names(model, model.used_by(&component.id));
            if !uses.is_empty() {
                card = card.child(relation("Uses", uses, cx));
            }
            if !used_by.is_empty() {
                card = card.child(relation("Used by", used_by, cx));
            }
        }

        let status: Option<SharedString> = if explaining {
            Some(if stale {
                "Changed since this explanation — explaining again…".into()
            } else {
                "Explaining with the model…".into()
            })
        } else if let Some(error) = error {
            Some(error.clone().into())
        } else if stale {
            Some("Changed since this explanation.".into())
        } else {
            summary.filter(|s| !s.model.is_empty()).map(|s| {
                format!(
                    "Explained by {} · {} tokens in, {} out",
                    s.model,
                    compact(s.input_tokens as usize),
                    compact(s.output_tokens as usize)
                )
                .into()
            })
        };
        let can_explain = !explaining && (summary.is_none() || stale || error.is_some());
        let id = component.id.clone();
        card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(if error.is_some() && !explaining {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .children(status),
                )
                .child(
                    Button::new("architecture-details")
                        .label(if self.details { "Less" } else { "More" })
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.details = !this.details;
                            cx.notify();
                        })),
                )
                .when(
                    can_explain && !self.config.auto_summarize || error.is_some(),
                    |d| {
                        d.child(
                            Button::new("architecture-explain")
                                .label("Explain")
                                .xsmall()
                                .ghost()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.explain(id.clone(), true, cx)
                                })),
                        )
                    },
                ),
        )
        .into_any_element()
    }

    fn render_graph(
        &self,
        model: &ArchitectureModel,
        component: &Component,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let children: Vec<&Component> = component
            .children
            .iter()
            .filter_map(|id| model.get(id))
            .take(MAX_BOXES)
            .collect();
        if children.is_empty() {
            return div()
                .p_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("No parts on this level.")
                .into_any_element();
        }
        let index: HashMap<&str, usize> = children
            .iter()
            .enumerate()
            .map(|(ix, c)| (c.id.as_str(), ix))
            .collect();
        let edges: Vec<(usize, usize)> = model
            .level_edges(&component.id)
            .iter()
            .filter_map(|e| Some((*index.get(e.from.as_str())?, *index.get(e.to.as_str())?)))
            .collect();

        let width = f32::from(self.graph_width.get().unwrap_or(px(420.)));
        let usable = (width - 2. * PAD).max(MIN_BOX_WIDTH);
        let columns = (((usable + GAP_X) / (MIN_BOX_WIDTH + GAP_X)).floor() as usize).max(1);
        let layout = layout::layered(children.len(), &edges, columns);
        let box_width = ((usable - (layout.columns as f32 - 1.) * GAP_X) / layout.columns as f32)
            .clamp(MIN_BOX_WIDTH.min(usable), MAX_BOX_WIDTH);
        let grid_width = layout.columns as f32 * box_width + (layout.columns as f32 - 1.) * GAP_X;
        let left = PAD + ((usable - grid_width) / 2.).max(0.);
        let row_y = |row: usize| {
            let extra = match layout.isolated_from {
                Some(from) if row >= from => ISOLATED_GAP,
                _ => 0.,
            };
            PAD + row as f32 * (BOX_HEIGHT + GAP_Y) + extra
        };
        let boxes: Vec<Bounds<Pixels>> = layout
            .slots
            .iter()
            .map(|slot| {
                Bounds::new(
                    point(
                        px(left + slot.column * (box_width + GAP_X)),
                        px(row_y(slot.row)),
                    ),
                    gpui_kit::size(px(box_width), px(BOX_HEIGHT)),
                )
            })
            .collect();
        let height = row_y(layout.rows.saturating_sub(1)) + BOX_HEIGHT + PAD;

        let focus = self
            .hovered
            .as_deref()
            .and_then(|id| index.get(id).copied());
        let related: HashSet<usize> = focus
            .map(|f| {
                edges
                    .iter()
                    .filter(|(a, b)| *a == f || *b == f)
                    .flat_map(|(a, b)| [*a, *b])
                    .collect()
            })
            .unwrap_or_default();
        let colors = EdgeColors {
            normal: theme
                .muted_foreground
                .opacity(if focus.is_some() { 0.15 } else { 0.45 }),
            uses: theme.primary,
            used_by: theme.success,
        };
        // Edges implied by longer paths are left out, except the hovered
        // box's own.
        let mut shown_edges = layout::transitive_reduction(children.len(), &edges);
        if let Some(f) = focus {
            for edge in edges.iter().filter(|(a, b)| *a == f || *b == f) {
                if !shown_edges.contains(edge) {
                    shown_edges.push(*edge);
                }
            }
        }
        let graph_width = self.graph_width.clone();
        let edge_boxes = boxes.clone();
        let edges_for_paint = shown_edges;
        let edge_canvas = canvas(
            move |bounds, _, _| bounds,
            move |bounds, _, window, _| {
                // The layout follows the panel's width.
                if graph_width.get() != Some(bounds.size.width) {
                    graph_width.set(Some(bounds.size.width));
                    window.refresh();
                }
                paint_edges(
                    window,
                    bounds.origin,
                    &edge_boxes,
                    &edges_for_paint,
                    focus,
                    colors,
                );
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let isolated_label = layout.isolated_from.map(|row| {
            div()
                .absolute()
                .left(px(PAD))
                .top(px(row_y(row) - ISOLATED_GAP + 2.))
                .text_xs()
                .text_color(muted)
                .child("No dependencies on this level")
        });

        let mut graph = div()
            .relative()
            .w_full()
            .h(px(height))
            .child(edge_canvas)
            .children(isolated_label);
        for (ix, child) in children.iter().enumerate() {
            let state = match focus {
                Some(f) if f == ix => BoxState::Focused,
                Some(_) if related.contains(&ix) => BoxState::Related,
                Some(_) => BoxState::Dimmed,
                None => BoxState::Normal,
            };
            graph = graph.child(self.render_box(model, child, ix, boxes[ix], state, cx));
        }
        let hidden = component.children.len().saturating_sub(MAX_BOXES);
        div()
            .flex()
            .flex_col()
            .child(graph)
            .when(hidden > 0, |d| {
                let rest: Vec<&str> = component.children[MAX_BOXES..]
                    .iter()
                    .map(|id| model.name(id))
                    .collect();
                d.child(
                    div()
                        .px_3()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{hidden} more: {}", rest.join(", "))),
                )
            })
            .into_any_element()
    }

    fn render_box(
        &self,
        model: &ArchitectureModel,
        component: &Component,
        ix: usize,
        bounds: Bounds<Pixels>,
        state: BoxState,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let line = self.part_line(model, &component.id);
        let commented = self.comment_on(&component.id).is_some();
        let border = match state {
            BoxState::Focused => theme.primary,
            BoxState::Related => theme.primary.opacity(0.5),
            _ if commented => theme.warning,
            _ => theme.border,
        };
        // Shown on hover, and always once there is a comment.
        let comment_button = (commented || state == BoxState::Focused).then(|| {
            comment_button(
                ("architecture-comment", ix),
                format!("architecture-comment-{}", component.name),
                component.id.clone(),
                commented,
                cx,
            )
        });
        let id_for_hover = component.id.clone();
        let id_for_click = component.id.clone();
        let name = component.name.clone();
        let opens_level = component.kind != ComponentKind::File;
        div()
            .id(("architecture-box", ix))
            .debug_selector(move || format!("architecture-box-{name}"))
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .p_2()
            .flex()
            .flex_col()
            .gap_0p5()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(if component.kind == ComponentKind::Package {
                theme.secondary
            } else {
                theme.background
            })
            .when(state == BoxState::Dimmed, |d| d.opacity(0.55))
            .when(component.is_test, |d| d.border_dashed())
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(kind_icon(component, theme.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(component.name.clone()),
                    )
                    .children(comment_button)
                    .when(opens_level, |d| {
                        d.child(
                            gpui_kit::svg()
                                .flex_none()
                                .size(px(10.))
                                .path("icons/chevron_right.svg")
                                .text_color(theme.muted_foreground),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .line_clamp(2)
                    .text_color(theme.muted_foreground)
                    .child(line.unwrap_or_default()),
            )
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_hovered(&id_for_hover, *hovered, cx)
            }))
            .on_click(
                cx.listener(move |this, _, _, cx| this.on_box_clicked(id_for_click.clone(), cx)),
            )
    }

    /// Details of the last hovered box.
    fn render_inspector(
        &self,
        model: &ArchitectureModel,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let id = self.inspected.as_deref()?;
        let component = model.get(id)?;
        let theme = cx.theme();
        let own = self.summaries.get(id);
        let mut panel = div()
            .flex_none()
            .max_h(px(260.))
            .overflow_y_scrollbar()
            .border_t_1()
            .border_color(theme.border)
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(kind_icon(component, theme.muted_foreground))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(component.name.clone()),
                    )
                    .child(badge(component.kind.label(), cx))
                    .when(component.is_test, |d| d.child(badge("test", cx)))
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(size_label(component)),
                    ),
            );
        if let Some(line) = self.part_line(model, id) {
            panel = panel.child(div().text_xs().text_color(theme.foreground).child(line));
        }
        if let Some(comment) = self.comment_on(id) {
            panel = panel.child(comment_note(comment.text.clone(), cx));
        }
        if let Some(own) = own {
            if !own.responsibility.is_empty() {
                panel = panel.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(own.responsibility.clone()),
                );
            }
            if !own.constraints.is_empty() {
                panel = panel.child(section_title("Constraints", cx)).children(
                    own.constraints
                        .iter()
                        .map(|c| bullet(c.clone(), theme.foreground, cx)),
                );
            }
        }
        let uses = names(model, model.uses(id));
        let used_by = names(model, model.used_by(id));
        if !uses.is_empty() {
            panel = panel.child(relation("Uses", uses, cx));
        }
        if !used_by.is_empty() {
            panel = panel.child(relation("Used by", used_by, cx));
        }
        match own.filter(|s| !s.interfaces.is_empty()) {
            Some(own) => {
                panel = panel.child(section_title("Key interfaces", cx)).children(
                    own.interfaces
                        .iter()
                        .map(|i| interface_line(i.name.clone(), i.description.clone(), cx)),
                );
            }
            None if !component.interface.is_empty() => {
                panel = panel.child(section_title("Exposes", cx)).children(
                    component
                        .interface
                        .iter()
                        .take(12)
                        .map(|item| signature_line(item.clone(), cx)),
                );
                if component.interface.len() > 12 {
                    panel = panel.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} more", component.interface.len() - 12)),
                    );
                }
            }
            None => {}
        }
        panel = panel.child(div().text_xs().text_color(theme.muted_foreground).child(
            if component.kind == ComponentKind::File {
                "Click to open the file"
            } else {
                "Click to open this level"
            },
        ));
        Some(panel.into_any_element())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BoxState {
    Normal,
    Focused,
    Related,
    Dimmed,
}

#[derive(Clone, Copy)]
struct EdgeColors {
    normal: Hsla,
    uses: Hsla,
    used_by: Hsla,
}

/// Arrows from users to what they use: down a row by default, up for the
/// rare edge against the layering (a cycle), sideways within a row.
fn paint_edges(
    window: &mut Window,
    origin: Point<Pixels>,
    boxes: &[Bounds<Pixels>],
    edges: &[(usize, usize)],
    focus: Option<usize>,
    colors: EdgeColors,
) {
    // Highlighted edges paint last, on top.
    let mut ordered: Vec<&(usize, usize)> = edges.iter().collect();
    ordered.sort_by_key(|(a, b)| focus.is_some_and(|f| *a == f || *b == f));
    for &&(from, to) in &ordered {
        let (a, b) = (boxes[from], boxes[to]);
        let (color, width) = match focus {
            Some(f) if f == from => (colors.uses, 1.6),
            Some(f) if f == to => (colors.used_by, 1.6),
            _ => (colors.normal, 1.0),
        };
        let center = |r: Bounds<Pixels>| f32::from(r.origin.x + r.size.width / 2.);
        let (ax, bx) = (center(a), center(b));
        let spread = |dx: f32, w: Pixels| (dx * 0.2).clamp(-f32::from(w) / 3., f32::from(w) / 3.);
        let (start, end, c1, c2) = if a.origin.y < b.origin.y {
            let start = point(px(ax + spread(bx - ax, a.size.width)), a.bottom());
            let end = point(px(bx + spread(ax - bx, b.size.width)), b.top());
            let dy = ((end.y - start.y) * 0.5).max(px(12.));
            (
                start,
                end,
                point(start.x, start.y + dy),
                point(end.x, end.y - dy),
            )
        } else if a.origin.y > b.origin.y {
            let start = point(px(ax + spread(bx - ax, a.size.width)), a.top());
            let end = point(px(bx + spread(ax - bx, b.size.width)), b.bottom());
            let dy = ((start.y - end.y) * 0.5).max(px(12.));
            (
                start,
                end,
                point(start.x, start.y - dy),
                point(end.x, end.y + dy),
            )
        } else {
            let y = a.origin.y + a.size.height / 2.;
            let (sx, ex) = if ax < bx {
                (a.right(), b.left())
            } else {
                (a.left(), b.right())
            };
            let lift = px(18.);
            (
                point(sx, y),
                point(ex, y),
                point(sx, y - lift),
                point(ex, y - lift),
            )
        };
        let shift = |p: Point<Pixels>| p + origin;
        let (start, end, c1, c2) = (shift(start), shift(end), shift(c1), shift(c2));
        let mut path = PathBuilder::stroke(px(width));
        path.move_to(start);
        path.cubic_bezier_to(end, c1, c2);
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
        // Arrowhead along the curve's end tangent.
        let (dx, dy) = (f32::from(end.x - c2.x), f32::from(end.y - c2.y));
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let (ux, uy) = (dx / len, dy / len);
        let size = 6.;
        let back = point(end.x - px(ux * size), end.y - px(uy * size));
        let left = point(back.x - px(uy * size * 0.5), back.y + px(ux * size * 0.5));
        let right = point(back.x + px(uy * size * 0.5), back.y - px(ux * size * 0.5));
        let mut head = PathBuilder::fill();
        head.add_polygon(&[end, left, right], true);
        if let Ok(head) = head.build() {
            window.paint_path(head, color);
        }
    }
}

/// The component at or above `id` that still exists.
fn nearest_existing(model: &ArchitectureModel, id: &str) -> String {
    let mut id = id;
    while !id.is_empty() {
        if model.get(id).is_some() {
            return id.to_owned();
        }
        id = id.rfind('/').map_or("", |i| &id[..i]);
    }
    String::new()
}

fn first_sentence(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.find(". ") {
        Some(ix) if ix < 200 => text[..=ix].to_owned(),
        _ => text.chars().take(200).collect(),
    }
}

fn compact(n: usize) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.)
    } else {
        n.to_string()
    }
}

fn size_label(component: &Component) -> String {
    let files = if component.file_count == 1 {
        "1 file".to_owned()
    } else {
        format!("{} files", component.file_count)
    };
    format!("{files} · {} lines", compact(component.line_count))
}

fn names(model: &ArchitectureModel, ids: Vec<&str>) -> Vec<String> {
    ids.into_iter()
        .map(|id| model.name(id).to_owned())
        .collect()
}

fn kind_icon(component: &Component, color: Hsla) -> gpui_kit::AnyElement {
    match component.kind {
        ComponentKind::File => {
            let icon = file_icons::get().get_icon_for_filename(&component.name);
            file_icons::render_icon(&icon, 13.0, color, "·").into_any_element()
        }
        ComponentKind::Module => {
            let icon = file_icons::get().get_type_icon("collapsed_folder");
            file_icons::render_icon(&icon, 13.0, color, "·").into_any_element()
        }
        ComponentKind::Package | ComponentKind::Project => Icon::default()
            .path(SharedString::from(
                if component.kind == ComponentKind::Project {
                    "icons/list_tree.svg"
                } else {
                    "icons/library.svg"
                },
            ))
            .with_size(Size::XSmall)
            .text_color(color)
            .into_any_element(),
    }
}

/// The button commenting on component `id`: a plus while it has no
/// comment, the comment mark after.
fn comment_button(
    element_id: impl Into<gpui_kit::ElementId>,
    selector: String,
    id: String,
    commented: bool,
    cx: &Context<ArchitectureView>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(element_id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .size(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|s| s.bg(theme.muted))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(if commented {
                "Edit comment"
            } else {
                "Comment"
            })
            .build(window, cx)
        })
        .child(
            gpui_kit::svg()
                .size(px(12.))
                .path(if commented {
                    "icons/message_bubbles.svg"
                } else {
                    "icons/message_circle_plus.svg"
                })
                .text_color(if commented {
                    theme.warning
                } else {
                    theme.muted_foreground
                }),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            // Not a click on the box.
            cx.stop_propagation();
            let position = window.mouse_position();
            this.start_comment(&id, position, window, cx);
        }))
}

/// The user's comment on a component, as the card and inspector show it.
fn comment_note(text: String, cx: &App) -> impl IntoElement {
    div()
        .pl_2()
        .border_l_2()
        .border_color(cx.theme().warning.opacity(0.6))
        .text_xs()
        .text_color(cx.theme().foreground)
        .child(text)
}

fn badge(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .flex_none()
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(cx.theme().border)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn section_title(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .pt_1()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn bullet(text: String, color: Hsla, cx: &App) -> impl IntoElement {
    div()
        .flex()
        .gap_1p5()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("•"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(color)
                .child(text),
        )
}

fn interface_line(name: String, description: String, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .text_xs()
        .flex()
        .flex_wrap()
        .gap_x_1()
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.foreground)
                .child(name),
        )
        .when(!description.is_empty(), |d| {
            d.child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("— {description}")),
            )
        })
}

fn signature_line(item: String, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .text_xs()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .font_family(theme.mono_font_family.clone())
        .text_color(theme.foreground)
        .child(item)
}

fn relation(label: &'static str, names: Vec<String>, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .flex()
        .gap_1()
        .child(
            div()
                .flex_none()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{label}:")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(cx.theme().foreground)
                .child(names.join(", ")),
        )
}

impl Focusable for ArchitectureView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ArchitectureView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A revealed comment opens where the user clicked to reveal it.
        if let Some(comment) = self.comment_request.take() {
            let position = window.mouse_position();
            self.open_comment_editor(comment, position, window, cx);
        }
        let editor = self
            .comment_editor
            .as_ref()
            .map(|(editor, position)| crate::comments::floating(*position, editor.clone()));
        let theme = cx.theme();
        let message = |text: String| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let body = match (&self.model, &self.scan_error) {
            (None, Some(error)) => message(error.clone()),
            (None, None) if self.session_id.is_none() => message("No session".into()),
            (None, None) => message("Scanning the project…".into()),
            (Some(model), _) => {
                let model = model.clone();
                match model.get(&self.current) {
                    None => message("This component is gone.".into()),
                    Some(component) => {
                        let about = self.render_about(&model, component, cx);
                        let graph = self.render_graph(&model, component, cx);
                        let inspector = self.render_inspector(&model, cx);
                        let muted = cx.theme().muted_foreground;
                        let truncated = model.truncated.then(|| {
                            div()
                                .px_3()
                                .text_xs()
                                .text_color(muted)
                                .child("The project has more files than the scan reads.")
                        });
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scrollbar()
                                    .child(div().p_3().pb_0().child(about))
                                    .children(truncated)
                                    .child(graph),
                            )
                            .children(inspector)
                            .into_any_element()
                    }
                }
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .child(self.render_toolbar(cx))
            .child(body)
            .children(editor)
    }
}

#[cfg(test)]
impl ArchitectureView {
    /// The component whose parts are shown.
    pub fn current(&self) -> &str {
        &self.current
    }

    /// The shown boxes, each with its one-line responsibility.
    pub fn shown_parts(&self) -> Vec<(String, String)> {
        let Some(model) = &self.model else {
            return Vec::new();
        };
        let Some(component) = model.get(&self.current) else {
            return Vec::new();
        };
        component
            .children
            .iter()
            .filter_map(|id| {
                let line = self.part_line(model, id).unwrap_or_default();
                Some((model.get(id)?.name.clone(), line))
            })
            .collect()
    }

    pub fn summary(&self, id: &str) -> Option<&ComponentSummary> {
        self.summaries.get(id)
    }

    pub fn comment_editor_open(&self) -> bool {
        self.comment_editor.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(3400), "3.4k");
        assert_eq!(compact(72_371), "72k");
        assert_eq!(
            first_sentence("Talks to models. And more."),
            "Talks to models."
        );
    }
}
