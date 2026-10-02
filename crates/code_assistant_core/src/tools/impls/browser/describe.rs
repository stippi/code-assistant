//! One-line descriptions of browser tool calls, shared by the frontends so a
//! call reads the same everywhere ("Click ref_4", "Find \"Sign in\"").

use serde_json::Value;

/// Every browser tool name, for frontends that register renderers by name.
pub const BROWSER_TOOL_NAMES: [&str; 18] = [
    "browser_navigate",
    "browser_computer",
    "browser_read_page",
    "browser_find",
    "browser_get_page_text",
    "browser_form_input",
    "browser_javascript",
    "browser_read_console_messages",
    "browser_read_network_requests",
    "browser_resize_window",
    "browser_tabs_context",
    "browser_tabs_create",
    "browser_tabs_select",
    "browser_tabs_close",
    "browser_batch",
    "browser_close",
    "browser_login",
    "browser_profiles",
];

/// Whether the output of `tool` is structured text (a tree, log lines) that
/// reads best in a monospace font.
pub fn has_structured_output(tool: &str) -> bool {
    matches!(
        tool,
        "browser_read_page"
            | "browser_find"
            | "browser_read_console_messages"
            | "browser_read_network_requests"
            | "browser_javascript"
            | "browser_tabs_context"
    )
}

/// Describe a browser tool call from its parameters. `param` returns a
/// parameter's value as shown in the UI (strings unquoted, arrays/objects as
/// JSON), or `None` when it is absent or still streaming.
pub fn describe_call(tool: &str, param: &dyn Fn(&str) -> Option<String>) -> String {
    let text = |name: &str, max: usize| param(name).map(|v| truncate(&v, max));
    match tool {
        "browser_navigate" => match param("url").as_deref() {
            Some("back") => "Go back".to_string(),
            Some("forward") => "Go forward".to_string(),
            Some(url) => format!("Navigate to {}", truncate(url, 70)),
            None => "Navigate".to_string(),
        },
        "browser_computer" => describe_computer(param),
        "browser_read_page" => {
            let mut out = if param("filter").as_deref() == Some("interactive") {
                "Read interactive elements".to_string()
            } else {
                "Read page".to_string()
            };
            if let Some(r) = param("ref_id") {
                out.push_str(&format!(" under {r}"));
            }
            out
        }
        "browser_find" => match text("query", 50) {
            Some(q) => format!("Find \"{q}\""),
            None => "Find on the page".to_string(),
        },
        "browser_get_page_text" => "Read page text".to_string(),
        "browser_form_input" => match (param("ref"), text("value", 40)) {
            (Some(r), Some(v)) => format!("Set {r} to {v}"),
            (Some(r), None) => format!("Set {r}"),
            _ => "Set form field".to_string(),
        },
        "browser_javascript" => match param("text") {
            Some(code) => format!(
                "Run {}",
                truncate(code.lines().next().unwrap_or_default().trim(), 60)
            ),
            None => "Run JavaScript".to_string(),
        },
        "browser_read_console_messages" => {
            if param("only_errors").as_deref() == Some("true") {
                "Read console errors".to_string()
            } else {
                "Read console messages".to_string()
            }
        }
        "browser_read_network_requests" => match param("request_id") {
            Some(id) => format!("Read response {id}"),
            None => "Read network requests".to_string(),
        },
        "browser_resize_window" => {
            let size = match (param("preset"), param("width"), param("height")) {
                (Some(preset), _, _) => Some(preset),
                (None, Some(w), Some(h)) => Some(format!("{w}×{h}")),
                _ => None,
            };
            match (size, param("color_scheme")) {
                (Some(size), Some(scheme)) => format!("Resize to {size}, {scheme} mode"),
                (Some(size), None) => format!("Resize to {size}"),
                (None, Some(scheme)) => format!("Switch to {scheme} mode"),
                (None, None) => "Resize viewport".to_string(),
            }
        }
        "browser_tabs_context" => "List tabs".to_string(),
        "browser_tabs_create" => "Open a tab".to_string(),
        "browser_tabs_select" => match param("tab_id") {
            Some(id) => format!("Switch to tab {id}"),
            None => "Switch tab".to_string(),
        },
        "browser_tabs_close" => match param("tab_id") {
            Some(id) => format!("Close tab {id}"),
            None => "Close tab".to_string(),
        },
        "browser_batch" => match param("actions")
            .and_then(|json| serde_json::from_str::<Value>(&json).ok())
            .and_then(|v| v.as_array().map(Vec::len))
        {
            Some(n) => format!("Browser steps ({n} step{})", if n == 1 { "" } else { "s" }),
            None => "Browser steps".to_string(),
        },
        "browser_close" => "Close browser".to_string(),
        "browser_login" => match param("url") {
            Some(url) => format!("Log in at {}", truncate(&url, 60)),
            None => "Log in".to_string(),
        },
        "browser_profiles" => "List browser profiles".to_string(),
        other => other.to_string(),
    }
}

/// The steps of a `browser_batch` call, each described like a single call.
pub fn describe_batch_steps(actions_json: &str) -> Vec<String> {
    let Ok(Value::Array(steps)) = serde_json::from_str::<Value>(actions_json) else {
        return Vec::new();
    };
    steps
        .iter()
        .map(|step| {
            let name = step.get("name").and_then(Value::as_str).unwrap_or("");
            let tool = if name.starts_with("browser_") {
                name.to_string()
            } else {
                format!("browser_{name}")
            };
            let input = step.get("input").cloned().unwrap_or(Value::Null);
            describe_call(&tool, &|key| {
                input.get(key).map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            })
        })
        .collect()
}

/// A short label for where a call runs: a non-default profile and/or a tab.
pub fn target_label(param: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let profile = param("profile").filter(|p| !p.is_empty() && p != "default");
    let tab = param("tab_id").filter(|t| !t.is_empty());
    match (profile, tab) {
        (Some(p), Some(t)) => Some(format!("{p} · {t}")),
        (Some(p), None) => Some(p),
        (None, Some(t)) => Some(t),
        (None, None) => None,
    }
}

fn describe_computer(param: &dyn Fn(&str) -> Option<String>) -> String {
    let target = param("ref")
        .or_else(|| param("coordinate").map(|c| compact_coordinate(&c)))
        .map(|t| format!(" {t}"))
        .unwrap_or_default();
    let modifiers = param("modifiers")
        .map(|m| format!("{m}+"))
        .unwrap_or_default();
    match param("action").as_deref() {
        Some("left_click") => format!("{modifiers}Click{target}"),
        Some("right_click") => format!("Right-click{target}"),
        Some("double_click") => format!("Double-click{target}"),
        Some("triple_click") => format!("Triple-click{target}"),
        Some("hover") => format!("Hover{target}"),
        Some("scroll_to") => format!("Scroll to{target}"),
        Some("left_click_drag") => "Drag".to_string(),
        Some("type") => match param("text") {
            Some(text) => format!("Type \"{}\"", truncate(&text, 40)),
            None => "Type".to_string(),
        },
        Some("key") => match param("text") {
            Some(keys) => {
                let times = param("repeat")
                    .filter(|r| r != "1")
                    .map(|r| format!(" ×{r}"))
                    .unwrap_or_default();
                format!("Press {}{times}", truncate(&keys, 40))
            }
            None => "Press keys".to_string(),
        },
        Some("scroll") => match param("scroll_direction") {
            Some(direction) => format!("Scroll {direction}"),
            None => "Scroll".to_string(),
        },
        Some("hold_key") => {
            let duration = param("duration")
                .map(|d| format!(" for {d}s"))
                .unwrap_or_default();
            format!("Hold {}{duration}", param("text").unwrap_or_default())
        }
        Some("key_down") => format!("Hold down {}", param("text").unwrap_or_default()),
        Some("key_up") => format!("Release {}", param("text").unwrap_or_default()),
        Some("left_mouse_down") => format!("Press the mouse{target}"),
        Some("left_mouse_up") => format!("Release the mouse{target}"),
        Some("screenshot") => "Screenshot".to_string(),
        Some("zoom") => "Zoom in".to_string(),
        Some("wait") => match param("duration") {
            Some(s) => format!("Wait {s}s"),
            None => "Wait".to_string(),
        },
        _ => "Use the browser".to_string(),
    }
}

/// `[120, 340]` (or `[120.0,340.5]`) → `(120, 340)`.
fn compact_coordinate(json: &str) -> String {
    match serde_json::from_str::<Vec<f64>>(json) {
        Ok(v) if v.len() == 2 => format!("({}, {})", v[0].round(), v[1].round()),
        _ => json.to_string(),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() > max_chars {
        let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn describe(tool: &str, params: &[(&str, &str)]) -> String {
        describe_call(tool, &|key| {
            params
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn computer_calls_name_the_action_and_target() {
        assert_eq!(
            describe(
                "browser_computer",
                &[("action", "left_click"), ("ref", "ref_4")]
            ),
            "Click ref_4"
        );
        assert_eq!(
            describe(
                "browser_computer",
                &[
                    ("action", "left_click"),
                    ("coordinate", "[120.4, 340]"),
                    ("modifiers", "ctrl")
                ]
            ),
            "ctrl+Click (120, 340)"
        );
        assert_eq!(
            describe(
                "browser_computer",
                &[("action", "key"), ("text", "Backspace"), ("repeat", "3")]
            ),
            "Press Backspace ×3"
        );
        assert_eq!(
            describe("browser_computer", &[("action", "screenshot")]),
            "Screenshot"
        );
        assert_eq!(
            describe(
                "browser_computer",
                &[("action", "hold_key"), ("text", "w"), ("duration", "0.5")]
            ),
            "Hold w for 0.5s"
        );
    }

    #[test]
    fn page_tools_describe_their_input() {
        assert_eq!(
            describe("browser_navigate", &[("url", "https://x.test")]),
            "Navigate to https://x.test"
        );
        assert_eq!(describe("browser_navigate", &[("url", "back")]), "Go back");
        assert_eq!(
            describe("browser_read_page", &[("filter", "interactive")]),
            "Read interactive elements"
        );
        assert_eq!(
            describe("browser_find", &[("query", "Sign in")]),
            "Find \"Sign in\""
        );
        assert_eq!(
            describe(
                "browser_form_input",
                &[("ref", "ref_7"), ("value", "Green")]
            ),
            "Set ref_7 to Green"
        );
        assert_eq!(
            describe("browser_javascript", &[("text", "document.title\n// more")]),
            "Run document.title"
        );
        assert_eq!(
            describe(
                "browser_resize_window",
                &[("preset", "mobile"), ("color_scheme", "dark")]
            ),
            "Resize to mobile, dark mode"
        );
    }

    #[test]
    fn batch_counts_and_describes_its_steps() {
        let json = r#"[{"name":"browser_computer","input":{"action":"left_click","ref":"ref_4"}},
                       {"name":"find","input":{"query":"Save"}}]"#;
        assert_eq!(
            describe("browser_batch", &[("actions", json)]),
            "Browser steps (2 steps)"
        );
        assert_eq!(
            describe_batch_steps(json),
            vec!["Click ref_4".to_string(), "Find \"Save\"".to_string()]
        );
    }

    #[test]
    fn target_label_shows_a_named_profile_and_tab() {
        let label = |params: &[(&str, &str)]| {
            target_label(&|key| {
                params
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            })
        };
        assert_eq!(label(&[("profile", "default")]), None);
        assert_eq!(
            label(&[("profile", "elster"), ("tab_id", "t2")]).as_deref(),
            Some("elster · t2")
        );
        assert_eq!(label(&[("tab_id", "t2")]).as_deref(), Some("t2"));
    }

    #[test]
    fn every_registered_browser_tool_has_a_name_here() {
        let mut registry = crate::tools::core::ToolRegistry::new();
        super::super::register(&mut registry);
        let mut registered: Vec<String> = registry
            .get_tool_definitions_with_capability(crate::tools::scope::ToolScope::Agent.tag())
            .into_iter()
            .map(|d| d.name)
            .collect();
        registered.sort();
        let mut listed: Vec<String> = BROWSER_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
        listed.sort();
        assert_eq!(registered, listed);
    }
}
