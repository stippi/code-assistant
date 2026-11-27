//! Requests to show a project file in the right panel's Files view, from
//! anywhere in the UI (e.g. a file path in a tool card).
//!
//! Elements deep in the transcript cannot reach the right panel directly:
//! they emit an [`OpenFileRequest`] on the app-wide [`OpenFileBus`], which
//! the main screen subscribes to.

use gpui_kit::{
    App, Div, Entity, EventEmitter, Global, InteractiveElement, Stateful,
    StatefulInteractiveElement,
};

/// Show `path` (relative to the project root, or absolute inside it) and
/// select `line` (1-based) when given. `project` names the project the
/// path belongs to, when the source knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFileRequest {
    pub path: String,
    pub line: Option<usize>,
    pub project: Option<String>,
}

pub struct OpenFileBus;

impl EventEmitter<OpenFileRequest> for OpenFileBus {}

#[derive(Clone)]
pub struct OpenFileGlobal(pub Entity<OpenFileBus>);

impl Global for OpenFileGlobal {}

/// Ask for `request` to be shown; a no-op before the main screen exists.
pub fn request(request: OpenFileRequest, cx: &mut App) {
    if let Some(bus) = cx.try_global::<OpenFileGlobal>().map(|g| g.0.clone()) {
        bus.update(cx, |_, cx| cx.emit(request));
    }
}

/// Make `element` open `request` on click, with a pointer cursor and an
/// underline on hover.
pub fn clickable(element: Stateful<Div>, request: OpenFileRequest) -> Stateful<Div> {
    use gpui_kit::Styled;
    element
        .cursor_pointer()
        .hover(|s| s.underline())
        .on_click(move |_, _, cx| self::request(request.clone(), cx))
}

/// Where a link in a chat message points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// A local file: relative to the project root, or absolute.
    File { path: String, line: Option<usize> },
    /// Anything with a URL scheme, for the system to open.
    Url(String),
    /// Nothing to open, e.g. an in-page anchor like `#usage`.
    None,
}

/// Classify a Markdown link from a chat message.
///
/// Models often link the files they wrote with a plain path such as
/// `analysis/table.md`. Handing that to the operating system as a URL fails
/// (macOS reports error -50), so a link without a URL scheme is a file. A
/// `#L12` fragment or a `:12` suffix names the line to select.
pub fn link_target(link: &str) -> LinkTarget {
    let link = link.trim();
    if link.is_empty() || link.starts_with('#') {
        return LinkTarget::None;
    }
    if let Some(rest) = link.strip_prefix("file://") {
        // `file:///abs/path` or `file://localhost/abs/path`.
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        return file_target(rest);
    }
    if has_url_scheme(link) {
        return LinkTarget::Url(link.to_owned());
    }
    file_target(link)
}

/// Open a chat message link: a file in the Files view, a URL in the
/// system's default app.
pub fn open_link(link: &str, cx: &mut App) {
    match link_target(link) {
        LinkTarget::File { path, line } => request(
            OpenFileRequest {
                path,
                line,
                project: None,
            },
            cx,
        ),
        LinkTarget::Url(url) => cx.open_url(&url),
        LinkTarget::None => {}
    }
}

/// Whether `link` starts with a URL scheme (`https:`, `mailto:`, ...).
/// A Windows drive letter (`C:\`) and a line suffix (`main.rs:12`) are not
/// schemes.
fn has_url_scheme(link: &str) -> bool {
    let Some((scheme, rest)) = link.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    let drive_letter = scheme.len() == 1;
    let line_suffix = !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    valid && !drive_letter && !line_suffix
}

/// A file target from a path with an optional `?query`, `#L12` fragment or
/// `:12` line suffix.
fn file_target(link: &str) -> LinkTarget {
    let (path, fragment) = match link.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (link, None),
    };
    let path = path.split('?').next().unwrap_or_default();
    let mut line = fragment.and_then(fragment_line);
    let path = match path.rsplit_once(':') {
        Some((file, suffix)) if !file.is_empty() && !suffix.is_empty() => {
            match suffix.parse::<usize>() {
                Ok(n) => {
                    line = line.or(Some(n));
                    file
                }
                Err(_) => path,
            }
        }
        _ => path,
    };
    let path = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    let path = path.trim_start_matches("./");
    if path.is_empty() {
        return LinkTarget::None;
    }
    LinkTarget::File {
        path: path.to_owned(),
        line: line.filter(|&n| n > 0),
    }
}

/// The line of a `L12` or `L12-L20` fragment.
fn fragment_line(fragment: &str) -> Option<usize> {
    let digits: String = fragment
        .strip_prefix('L')?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, line: Option<usize>) -> LinkTarget {
        LinkTarget::File {
            path: path.to_owned(),
            line,
        }
    }

    #[test]
    fn relative_paths_are_files() {
        assert_eq!(
            link_target("analysis/table.md"),
            file("analysis/table.md", None)
        );
        assert_eq!(
            link_target("./analysis/table.csv"),
            file("analysis/table.csv", None)
        );
        assert_eq!(link_target("README.md"), file("README.md", None));
        assert_eq!(link_target("my%20notes.md"), file("my notes.md", None));
    }

    #[test]
    fn absolute_paths_are_files() {
        assert_eq!(
            link_target("/repo/src/lib.rs"),
            file("/repo/src/lib.rs", None)
        );
        assert_eq!(
            link_target("file:///repo/src/lib.rs"),
            file("/repo/src/lib.rs", None)
        );
        assert_eq!(
            link_target("file://localhost/repo/a%20b.rs"),
            file("/repo/a b.rs", None)
        );
        assert_eq!(
            link_target(r"C:\repo\lib.rs"),
            file(r"C:\repo\lib.rs", None)
        );
    }

    #[test]
    fn line_numbers_are_kept() {
        assert_eq!(
            link_target("src/main.rs#L42"),
            file("src/main.rs", Some(42))
        );
        assert_eq!(
            link_target("src/main.rs#L42-L50"),
            file("src/main.rs", Some(42))
        );
        assert_eq!(link_target("src/main.rs:42"), file("src/main.rs", Some(42)));
        assert_eq!(link_target("main.rs:7"), file("main.rs", Some(7)));
        assert_eq!(link_target("notes.md#usage"), file("notes.md", None));
    }

    #[test]
    fn urls_go_to_the_system() {
        for url in [
            "https://example.com/a.md",
            "http://localhost:8080/",
            "mailto:someone@example.com",
            "vscode://file/x",
        ] {
            assert_eq!(link_target(url), LinkTarget::Url(url.to_owned()));
        }
    }

    #[test]
    fn anchors_open_nothing() {
        assert_eq!(link_target("#usage"), LinkTarget::None);
        assert_eq!(link_target(""), LinkTarget::None);
    }
}
