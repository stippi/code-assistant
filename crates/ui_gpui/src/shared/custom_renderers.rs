//! File renderers the user defines, of two kinds: a command that turns a
//! file into an SVG or an image, which the Files view shows; or a web
//! editor, a web app editing the file in place. Stored in
//! `<config_dir>/file-renderers.json` and edited in *Settings → File
//! Renderers*; see `docs/file-renderers.md`.
//!
//! This module holds the configuration and runs commands; the views are
//! `file_renderers::command` and `file_renderers::web_editor`.

use anyhow::{Context as _, Result, bail};
use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::warn;

/// How long a command may run before it is stopped.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// Lines of a failed command's error output shown.
const ERROR_LINES: usize = 20;

/// What the command produces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// An SVG drawing, drawn sharp at any size.
    #[default]
    Svg,
    /// A raster image: PNG, JPEG, or another format the `image` crate reads.
    #[serde(alias = "png")]
    Image,
}

/// One user-defined renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomRenderer {
    /// The name in the viewer's dropdown; also identifies the renderer, so
    /// it is unique.
    pub label: String,
    /// Patterns for the file names it takes, e.g. `*.dot`; `*` and `?`
    /// are wildcards, case is ignored.
    pub files: Vec<String>,
    #[serde(flatten)]
    pub kind: RendererKind,
}

/// What a renderer does, by `"kind"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RendererKind {
    Command(CommandRenderer),
    Web(WebEditor),
}

/// A command that turns the file into an SVG or an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRenderer {
    /// A shell command line. `{file}` is the file's path, `{output}` the
    /// path to write the result to; without `{output}` the result is read
    /// from standard output. The file's text is also on standard input.
    pub command: String,
    #[serde(default)]
    pub output: OutputFormat,
}

/// A web app editing the file in place, talking the embed protocol (macOS
/// only for now; see `file_renderers::web_editor`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebEditor {
    /// The editor's address, with what it needs to talk the embed
    /// protocol; `{dark}` is `1` in a dark theme, else `0`.
    pub url: String,
    /// Scripts injected into the editor: paths, `~/` for the home folder,
    /// relative ones from the config folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scripts: Vec<String>,
}

/// The file a script entry names.
pub fn script_path(entry: &str) -> PathBuf {
    let entry = entry.trim();
    if let Some(rest) = entry.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    code_assistant_core::config_dir::config_dir().join(entry)
}

impl CustomRenderer {
    /// Whether this platform can show it.
    pub fn is_supported(&self) -> bool {
        match self.kind {
            RendererKind::Command(_) => true,
            RendererKind::Web(_) => cfg!(target_os = "macos"),
        }
    }

    /// The renderer id remembered for picks.
    pub fn id(&self) -> String {
        format!("custom:{}", self.label)
    }

    /// Whether it takes the file at the `/`-separated `path`.
    pub fn accepts(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
        self.files
            .iter()
            .any(|pattern| wildcard_match(&pattern.trim().to_lowercase(), &name))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomRenderersConfig {
    #[serde(default)]
    pub renderers: Vec<CustomRenderer>,
}

impl CustomRenderersConfig {
    pub fn path() -> PathBuf {
        code_assistant_core::config_dir::config_dir().join("file-renderers.json")
    }

    /// Load the config; a missing or malformed file has no renderers.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!(
                "Failed to parse {}: {e}; no custom renderers",
                path.display()
            );
            Self::default()
        })
    }

    pub fn save(&self) -> Result<()> {
        code_assistant_core::utils::file_utils::atomic_write_json(&Self::path(), self)
    }
}

/// The custom renderers as the Files view offers them.
pub struct CustomRenderersGlobal(pub Arc<Vec<CustomRenderer>>);

impl Global for CustomRenderersGlobal {}

/// The custom renderers currently configured.
pub fn current(cx: &App) -> Arc<Vec<CustomRenderer>> {
    cx.try_global::<CustomRenderersGlobal>()
        .map(|global| global.0.clone())
        .unwrap_or_default()
}

/// Offer `config`'s renderers from now on.
pub fn set_current(config: &CustomRenderersConfig, cx: &mut App) {
    cx.set_global(CustomRenderersGlobal(Arc::new(config.renderers.clone())));
}

/// Whether `name` matches `pattern`, where `*` stands for any run of
/// characters and `?` for one.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    // Greedy with backtracking to the last `*`.
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((sp, sn)) => {
                    p = sp + 1;
                    n = sn + 1;
                    star = Some((sp, sn + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// Quote `path` as one shell word.
fn quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The command line with its placeholders filled in.
fn command_line(template: &str, file: &Path, output: &Path) -> String {
    template
        .replace("{file}", &quote(file))
        .replace("{output}", &quote(output))
}

/// Run `renderer`'s command on the file at `file` (absolute) in the project at
/// `root`, whose current text is `text`. Returns what it produced.
///
/// When `text` differs from the file on disk (edits not saved yet), the
/// command gets a copy with the same name in a temporary folder instead.
/// Blocks until the command ends, `cancel` is set, or [`TIMEOUT`].
pub fn run(
    renderer: &CommandRenderer,
    file: &Path,
    root: &Path,
    text: &str,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    let scratch = tempfile::Builder::new()
        .prefix("code-assistant-render-")
        .tempdir()
        .context("Could not create a temporary folder")?;
    let on_disk = std::fs::read(file).ok();
    let input = if on_disk.as_deref() == Some(text.as_bytes()) {
        file.to_path_buf()
    } else {
        let copy = scratch
            .path()
            .join(file.file_name().unwrap_or("file".as_ref()));
        std::fs::write(&copy, text).context("Could not write the unsaved text")?;
        copy
    };
    let extension = match renderer.output {
        OutputFormat::Svg => "svg",
        OutputFormat::Image => "png",
    };
    let output = scratch.path().join(format!("output.{extension}"));
    let to_file = renderer.command.contains("{output}");
    let line = command_line(&renderer.command, &input, &output);

    let mut child = shell(&line)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Could not run `{line}`"))?;

    // Feed and drain the pipes on their own threads, so a full pipe never
    // blocks the command.
    let stdin = child.stdin.take().map(|mut pipe| {
        let text = text.to_owned();
        std::thread::spawn(move || {
            // A command that does not read its input closes the pipe early.
            pipe.write_all(text.as_bytes()).ok();
        })
    });
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let status = wait(&mut child, cancel)?;
    if let Some(thread) = stdin {
        thread.join().ok();
    }
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let lines: Vec<&str> = stderr.trim_end().lines().collect();
        let tail = lines[lines.len().saturating_sub(ERROR_LINES)..].join("\n");
        let code = status
            .code()
            .map_or_else(|| "was stopped".to_owned(), |c| format!("exited with {c}"));
        if tail.is_empty() {
            bail!("The command {code}");
        }
        bail!("The command {code}:\n{tail}");
    }
    let result = if to_file {
        std::fs::read(&output).context("The command wrote nothing to {output}")?
    } else {
        stdout
    };
    if result.is_empty() {
        let problem = if to_file {
            "The command wrote an empty file to {output}"
        } else {
            "The command printed nothing"
        };
        bail!("{problem}");
    }
    Ok(result)
}

/// `line` run by the platform's shell.
fn shell(line: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        // As typed: Rust's quoting of arguments is not what cmd expects.
        command.arg("/C").raw_arg(line);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-c").arg(line);
        command
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut bytes).ok();
        }
        bytes
    })
}

/// Wait for `child` to exit; stop it when `cancel` is set or it runs too
/// long.
fn wait(child: &mut Child, cancel: &AtomicBool) -> Result<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if cancel.load(Ordering::Relaxed) {
            child.kill().ok();
            child.wait().ok();
            bail!("Cancelled");
        }
        if started.elapsed() > TIMEOUT {
            child.kill().ok();
            child.wait().ok();
            bail!("The command did not finish within {} s", TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer(files: &[&str]) -> CustomRenderer {
        CustomRenderer {
            label: "test".into(),
            files: files.iter().map(|f| f.to_string()).collect(),
            kind: RendererKind::Web(WebEditor::default()),
        }
    }

    fn command(command: &str) -> CommandRenderer {
        CommandRenderer {
            command: command.into(),
            output: OutputFormat::Svg,
        }
    }

    #[test]
    fn file_patterns_match_names_ignoring_case() {
        let diagrams = renderer(&["*.diagram", " *.DGM "]);
        assert!(diagrams.accepts("docs/flow.diagram"));
        assert!(diagrams.accepts("FLOW.DIAGRAM"));
        assert!(diagrams.accepts("a/b/c.dgm"));
        assert!(!diagrams.accepts("flow.diagram.svg"));
        assert!(
            !diagrams.accepts("diagram/readme.md"),
            "only the name counts"
        );

        let double = renderer(&["*.diagram.svg", "diagram-?.txt", "Dockerfile"]);
        assert!(double.accepts("x/flow.diagram.svg"));
        assert!(!double.accepts("x/flow.svg"));
        assert!(double.accepts("diagram-1.txt"));
        assert!(!double.accepts("diagram-12.txt"));
        assert!(double.accepts("dockerfile"));
        assert!(wildcard_match("*a*b", "xxaybb"));
        assert!(!wildcard_match("*a*b", "xxaybc"));
        assert!(wildcard_match("*", ""));
    }

    #[test]
    fn the_config_reads_with_defaults() {
        let config: CustomRenderersConfig = serde_json::from_str(
            r#"{"renderers": [
                {"label": "png", "files": ["*.x"], "kind": "command", "command": "x", "output": "png"},
                {"label": "dot", "files": ["*.dot"], "kind": "command", "command": "dot -Tsvg"},
                {"label": "Plain", "files": ["*.a"], "kind": "web", "url": "https://a.example/"},
                {"label": "Shapes", "files": ["*.b"], "kind": "web",
                 "url": "https://b.example/?dark={dark}", "scripts": ["~/shapes.js"]}
            ]}"#,
        )
        .unwrap();
        let kinds: Vec<_> = config.renderers.iter().map(|r| r.kind.clone()).collect();
        assert_eq!(
            kinds,
            [
                RendererKind::Command(CommandRenderer {
                    command: "x".into(),
                    output: OutputFormat::Image,
                }),
                RendererKind::Command(command("dot -Tsvg")),
                RendererKind::Web(WebEditor {
                    url: "https://a.example/".into(),
                    scripts: Vec::new(),
                }),
                RendererKind::Web(WebEditor {
                    url: "https://b.example/?dark={dark}".into(),
                    scripts: vec!["~/shapes.js".into()],
                }),
            ]
        );
        assert_eq!(config.renderers[1].id(), "custom:dot");
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            json.contains(r#""kind":"command","command":"x","output":"image""#),
            "{json}"
        );
        assert!(
            json.contains(r#""kind":"web","url":"https://a.example/"}"#),
            "{json}"
        );
        let read: CustomRenderersConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(read, config);
        assert_eq!(
            CustomRenderersConfig::load_from(Path::new("/nonexistent/file-renderers.json")),
            CustomRenderersConfig::default()
        );
    }

    #[test]
    fn script_paths_resolve_home_and_config_folders() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(script_path(" ~/s/shapes.js "), home.join("s/shapes.js"));
        assert_eq!(
            script_path("/abs/shapes.js"),
            PathBuf::from("/abs/shapes.js")
        );
        assert!(script_path("scripts/shapes.js").ends_with("scripts/shapes.js"));
    }

    #[cfg(unix)]
    #[test]
    fn placeholders_are_quoted_paths() {
        let line = command_line(
            "tool -o {output} {file}",
            Path::new("/p/it's a.dot"),
            Path::new("/t/out.svg"),
        );
        assert_eq!(line, r#"tool -o '/t/out.svg' '/p/it'\''s a.dot'"#);
    }

    #[cfg(unix)]
    #[test]
    fn a_command_renders_through_output_file_or_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a b.txt");
        std::fs::write(&file, "on disk").unwrap();
        let cancel = AtomicBool::new(false);

        // Saved text: the command reads the file itself.
        let out = run(
            &command("cat {file} > {output}"),
            &file,
            dir.path(),
            "on disk",
            &cancel,
        )
        .unwrap();
        assert_eq!(out, b"on disk");

        // Unsaved text: a copy with the same name.
        let out = run(
            &command("basename {file}; cat {file}"),
            &file,
            dir.path(),
            "edited",
            &cancel,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a b.txt\nedited");

        // The text on standard input; run in the project root.
        let out = String::from_utf8(
            run(
                &command("tr a-z A-Z; pwd"),
                &file,
                dir.path(),
                "abc\n",
                &cancel,
            )
            .unwrap(),
        )
        .unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(out, format!("ABC\n{}\n", root.display()));
    }

    #[cfg(unix)]
    #[test]
    fn failures_say_why() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        let cancel = AtomicBool::new(false);
        let run = |command: &str| {
            run(&self::command(command), &file, dir.path(), "", &cancel)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            run("echo oops >&2; exit 3"),
            "The command exited with 3:\noops"
        );
        assert_eq!(run("true"), "The command printed nothing");
        assert_eq!(
            run("true {output}"),
            "The command wrote nothing to {output}"
        );

        let cancel = AtomicBool::new(true);
        let error = super::run(&command("sleep 5"), &file, dir.path(), "", &cancel).unwrap_err();
        assert_eq!(error.to_string(), "Cancelled");
    }
}
