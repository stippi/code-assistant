//! File renderers settings section — renderers the user defines for the
//! Files view: a command that turns a file into an SVG or an image, or a
//! web editor (with scripts injected into it). Backed by
//! `<config_dir>/file-renderers.json`; changes apply at once.

use crate::shared::custom_renderers::{
    self, CommandRenderer, CustomRenderer, CustomRenderersConfig, OutputFormat, RendererKind,
    WebEditor, script_path,
};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, SharedString, Window, div,
    prelude::*, px,
};
use tracing::warn;

/// Which kind of renderer the form edits.
#[derive(Clone, Copy, PartialEq)]
enum FormKind {
    Command,
    Web,
}

/// Whether this platform has web editors.
const WEB_SUPPORTED: bool = cfg!(target_os = "macos");

#[derive(Clone, Copy, PartialEq)]
enum FormMode {
    Hidden,
    Adding,
    /// Editing the renderer at this index.
    Editing(usize),
}

pub struct RenderersSection {
    focus_handle: FocusHandle,
    config: CustomRenderersConfig,
    form_mode: FormMode,
    form_kind: FormKind,
    form_output: OutputFormat,
    form_label_input: Entity<InputState>,
    form_files_input: Entity<InputState>,
    form_command_input: Entity<TextareaState>,
    form_url_input: Entity<InputState>,
    form_scripts_input: Entity<TextareaState>,
    /// Why the form could not be saved.
    form_error: Option<String>,
}

impl RenderersSection {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let form_label_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Graphviz"));
        let form_files_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. *.dot, *.gv"));
        let form_command_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("e.g. dot -Tsvg -o {output} {file}")
        });
        let form_url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("e.g. https://editor.example/?embed=1")
        });
        let form_scripts_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("one file per line, e.g. ~/editor/shapes.js")
        });
        Self {
            focus_handle: cx.focus_handle(),
            config: CustomRenderersConfig::load(),
            form_mode: FormMode::Hidden,
            form_kind: FormKind::Command,
            form_output: OutputFormat::Svg,
            form_label_input,
            form_files_input,
            form_command_input,
            form_url_input,
            form_scripts_input,
            form_error: None,
        }
    }

    /// Reload the configuration from disk.
    pub fn reload(&mut self) {
        self.config = CustomRenderersConfig::load();
        self.form_mode = FormMode::Hidden;
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.config.save() {
            warn!("Failed to save file-renderers.json: {e:#}");
        }
        custom_renderers::set_current(&self.config, cx);
        cx.notify();
    }

    fn open_form(
        &mut self,
        mode: FormMode,
        renderer: Option<CustomRenderer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let renderer = renderer.unwrap_or(CustomRenderer {
            label: String::new(),
            files: Vec::new(),
            kind: RendererKind::Command(CommandRenderer {
                command: String::new(),
                output: OutputFormat::Svg,
            }),
        });
        let (command, web) = match renderer.kind {
            RendererKind::Command(command) => {
                self.form_kind = FormKind::Command;
                (command, WebEditor::default())
            }
            RendererKind::Web(web) => {
                self.form_kind = FormKind::Web;
                let command = CommandRenderer {
                    command: String::new(),
                    output: OutputFormat::Svg,
                };
                (command, web)
            }
        };
        self.form_mode = mode;
        self.form_output = command.output;
        self.form_error = None;
        self.form_label_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(renderer.label), window, cx)
        });
        self.form_files_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(renderer.files.join(", ")), window, cx)
        });
        self.form_command_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(command.command), window, cx)
        });
        self.form_url_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(web.url), window, cx)
        });
        self.form_scripts_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(web.scripts.join("\n")), window, cx)
        });
        cx.notify();
    }

    /// The renderer the form describes, or why it is not one.
    fn form_renderer(&self, cx: &App) -> Result<CustomRenderer, String> {
        let label = self.form_label_input.read(cx).value().trim().to_string();
        let files: Vec<String> = self
            .form_files_input
            .read(cx)
            .value()
            .split([',', ' '])
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_string)
            .collect();
        if label.is_empty() {
            return Err("Give the renderer a name.".into());
        }
        let editing = match self.form_mode {
            FormMode::Editing(index) => Some(index),
            _ => None,
        };
        let taken = self
            .config
            .renderers
            .iter()
            .enumerate()
            .any(|(i, r)| Some(i) != editing && r.label.eq_ignore_ascii_case(&label));
        if taken {
            return Err(format!("There is a renderer named {label} already."));
        }
        if label.eq_ignore_ascii_case("text") {
            return Err("Text is the text editor's name.".into());
        }
        if files.is_empty() {
            return Err("Say which files it shows, e.g. *.dot.".into());
        }
        let kind = match self.form_kind {
            FormKind::Command => {
                // One line, however it was typed or pasted.
                let command = self
                    .form_command_input
                    .read(cx)
                    .value()
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if command.is_empty() {
                    return Err("Enter the command that renders a file.".into());
                }
                RendererKind::Command(CommandRenderer {
                    command,
                    output: self.form_output,
                })
            }
            FormKind::Web => {
                let url = self.form_url_input.read(cx).value().trim().to_string();
                if url.is_empty() {
                    return Err("Enter the editor's address.".into());
                }
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err("The editor's address starts with https:// or http://.".into());
                }
                let scripts: Vec<String> = self
                    .form_scripts_input
                    .read(cx)
                    .value()
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                if let Some(missing) = scripts.iter().find(|p| !script_path(p).is_file()) {
                    return Err(format!(
                        "There is no script file {}.",
                        script_path(missing).display()
                    ));
                }
                RendererKind::Web(WebEditor { url, scripts })
            }
        };
        Ok(CustomRenderer { label, files, kind })
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let renderer = match self.form_renderer(cx) {
            Ok(renderer) => renderer,
            Err(problem) => {
                self.form_error = Some(problem);
                cx.notify();
                return;
            }
        };
        match self.form_mode {
            FormMode::Editing(index) if index < self.config.renderers.len() => {
                self.config.renderers[index] = renderer;
            }
            _ => self.config.renderers.push(renderer),
        }
        self.form_mode = FormMode::Hidden;
        self.persist(cx);
    }

    fn delete(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.config.renderers.len() {
            self.config.renderers.remove(index);
        }
        self.form_mode = FormMode::Hidden;
        self.persist(cx);
    }

    fn render_card(
        &self,
        index: usize,
        renderer: &CustomRenderer,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let editing = self.form_mode == FormMode::Editing(index);
        let (shows, detail) = match &renderer.kind {
            RendererKind::Command(command) => (
                match command.output {
                    OutputFormat::Svg => "SVG",
                    OutputFormat::Image => "image",
                },
                command.command.clone(),
            ),
            RendererKind::Web(web) => (
                "web editor",
                match web.scripts.len() {
                    0 => web.url.clone(),
                    1 => format!("{}, 1 script", web.url),
                    n => format!("{}, {n} scripts", web.url),
                },
            ),
        };
        let unsupported = !renderer.is_supported();
        let edited = renderer.clone();
        card(cx)
            .child(
                div()
                    .id(SharedString::from(format!("renderer-header-{index}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.form_mode == FormMode::Editing(index) {
                            this.form_mode = FormMode::Hidden;
                            cx.notify();
                        } else {
                            this.open_form(
                                FormMode::Editing(index),
                                Some(edited.clone()),
                                window,
                                cx,
                            );
                        }
                    }))
                    .child(
                        Icon::default()
                            .path(SharedString::from(if editing {
                                "icons/chevron_down.svg"
                            } else {
                                "icons/chevron_right.svg"
                            }))
                            .with_size(Size::Small)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(cx.theme().foreground)
                                            .child(SharedString::from(renderer.label.clone())),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(SharedString::from(format!(
                                                "{} → {shows}{}",
                                                renderer.files.join(", "),
                                                if unsupported { " (macOS only)" } else { "" }
                                            ))),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(SharedString::from(detail)),
                            ),
                    ),
            )
            .when(editing, |el| el.child(self.render_form(cx)))
    }

    /// Name, files, kind, the kind's settings, and the actions.
    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = match self.form_mode {
            FormMode::Editing(index) => Some(index),
            _ => None,
        };
        let kind_pill = |label: &'static str, kind: FormKind, cx: &mut Context<Self>| {
            pill(
                format!("renderer-kind-{label}"),
                label,
                self.form_kind == kind,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.form_kind = kind;
                this.form_error = None;
                cx.notify();
            }))
        };
        let kinds = div()
            .flex()
            .items_center()
            .gap_2()
            .child(kind_pill("Command", FormKind::Command, cx))
            .when(WEB_SUPPORTED || self.form_kind == FormKind::Web, |el| {
                el.child(kind_pill("Web editor", FormKind::Web, cx))
            })
            .into_any_element();
        let is_command = self.form_kind == FormKind::Command;
        let output_pill = |label: &'static str, format: OutputFormat, cx: &mut Context<Self>| {
            pill(
                format!("renderer-output-{label}"),
                label,
                self.form_output == format,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.form_output = format;
                cx.notify();
            }))
        };
        let outputs = div()
            .flex()
            .items_center()
            .gap_2()
            .child(output_pill("SVG", OutputFormat::Svg, cx))
            .child(output_pill("Image (PNG, JPEG)", OutputFormat::Image, cx))
            .into_any_element();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pb_4()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(form_row(
                "Name",
                Input::new(&self.form_label_input).into_any_element(),
                cx,
            ))
            .child(form_row(
                "Files",
                Input::new(&self.form_files_input).into_any_element(),
                cx,
            ))
            .child(form_row("Kind", kinds, cx))
            .when(is_command, |el| {
                el.child(form_row(
                    "Command",
                    Textarea::new(&self.form_command_input).into_any_element(),
                    cx,
                ))
                .child(form_row("Output", outputs, cx))
                .child(hint(
                    "Files: name patterns, * and ? are wildcards. The command runs in the \
                     project folder through the shell. {file} is the file's path, {output} \
                     where to write the result; without {output} the result is read from \
                     standard output. The file's text is also on standard input.",
                    cx,
                ))
            })
            .when(!is_command, |el| {
                el.child(form_row(
                    "Address",
                    Input::new(&self.form_url_input).into_any_element(),
                    cx,
                ))
                .child(form_row(
                    "Scripts",
                    Textarea::new(&self.form_scripts_input).into_any_element(),
                    cx,
                ))
                .child(hint(
                    "Files: name patterns, * and ? are wildcards. The editor edits them in \
                     place and every change is saved. Address: a web editor talking the \
                     embed protocol (JSON over postMessage: init, load, autosave); {dark} \
                     becomes 1 in a dark theme, else 0. Scripts: files injected into the \
                     editor once it has loaded, ~/ for your home folder.",
                    cx,
                ))
            })
            .when_some(self.form_error.clone(), |el, problem| {
                el.child(
                    div()
                        .pl(px(92.))
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(SharedString::from(problem)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().when_some(editing, |el, index| {
                        el.child(
                            text_button("renderer-form-delete", "Delete", cx)
                                .text_color(cx.theme().danger)
                                .hover(|s| s.bg(gpui_kit::hsla(0.0, 0.7, 0.5, 0.1)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete(index, cx);
                                })),
                        )
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                text_button("renderer-form-cancel", "Cancel", cx)
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.form_mode = FormMode::Hidden;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                text_button("renderer-form-save", "Save", cx)
                                    .bg(cx.theme().primary)
                                    .text_color(cx.theme().primary_foreground)
                                    .hover(|s| s.opacity(0.9))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_form(cx);
                                    })),
                            ),
                    ),
            )
    }
}

fn hint(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .pl(px(92.))
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// A choice among a few, as a toggle pill.
fn pill(
    id: String,
    label: &'static str,
    selected: bool,
    cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(SharedString::from(id))
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .when(selected, |s| {
            s.bg(cx.theme().primary)
                .text_color(cx.theme().primary_foreground)
        })
        .when(!selected, |s| {
            s.text_color(cx.theme().muted_foreground)
                .hover(|s| s.bg(cx.theme().muted))
        })
        .child(label)
}

fn card(cx: &App) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
}

fn text_button(
    id: &'static str,
    label: &'static str,
    _cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .child(label)
}

fn form_row(label: &str, widget: AnyElement, cx: &App) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(80.))
                .flex_none()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(label.to_string())),
        )
        .child(div().flex_1().min_w_0().child(widget))
}

impl Focusable for RenderersSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RenderersSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let renderers = self.config.renderers.clone();
        let adding = self.form_mode == FormMode::Adding;

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .max_w(px(700.))
            .mx_auto()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child("FILE RENDERERS"),
                    )
                    .child(
                        div().flex().items_center().gap_2().child(
                            div()
                                .id("renderer-add-btn")
                                .flex()
                                .items_center()
                                .gap_1()
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .bg(cx.theme().primary)
                                .hover(|s| s.bg(cx.theme().primary.opacity(0.8)))
                                .child(
                                    Icon::default()
                                        .path(SharedString::from("icons/plus.svg"))
                                        .with_size(Size::XSmall)
                                        .text_color(cx.theme().primary_foreground),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().primary_foreground)
                                        .child("Add"),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_form(FormMode::Adding, None, window, cx);
                                })),
                        ),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "The Files view shows a file with a renderer instead of as text: \
                         Markdown rendered and SVG drawn are built in. Add your own: a \
                         command that turns a file into an SVG or an image (Graphviz, \
                         PlantUML), or a web editor, which edits files in place (macOS). \
                         Pick between them above the file.",
                    ),
            )
            .when(adding, |el| {
                el.child(
                    card(cx)
                        .child(
                            div()
                                .px_4()
                                .py_3()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .child("New renderer"),
                        )
                        .child(self.render_form(cx)),
                )
            })
            .when(renderers.is_empty() && !adding, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .py_8()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No renderers of your own yet"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(format!(
                                    "They are stored in {}",
                                    CustomRenderersConfig::path().display()
                                ))),
                        ),
                )
            })
            .child(
                div().flex().flex_col().gap_2().children(
                    renderers
                        .iter()
                        .enumerate()
                        .map(|(i, r)| self.render_card(i, r, cx).into_any_element()),
                ),
            )
    }
}
