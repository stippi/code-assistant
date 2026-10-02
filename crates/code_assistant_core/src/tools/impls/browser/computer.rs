//! `browser_computer`: mouse, keyboard, screenshots — by `ref_N` or by
//! coordinates in the latest screenshot's frame.

use super::{BrowserOutput, Resolved, Target, browser_tool, spec};
use crate::tools::core::ToolSpec;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use web::{BrowserSessionManager, Button, Tab};

/// Pixels one wheel tick scrolls.
const TICK_PX: f64 = 100.0;

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComputerAction {
    LeftClick,
    RightClick,
    DoubleClick,
    TripleClick,
    Type,
    Key,
    Screenshot,
    Wait,
    Scroll,
    ScrollTo,
    Hover,
    LeftClickDrag,
    Zoom,
    HoldKey,
    KeyDown,
    KeyUp,
    LeftMouseDown,
    LeftMouseUp,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Deserialize, Serialize)]
pub struct ComputerInput {
    pub action: ComputerAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_coordinate: Option<[f64; 2]>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_direction: Option<ScrollDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_amount: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accept_dialogs: bool,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserComputerTool;

impl BrowserComputerTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_computer",
            "Mouse, keyboard and screenshots in a browser tab. Target elements by `ref` (from \
             browser_read_page/browser_find) or by `coordinate` in the pixel frame of the most \
             recent screenshot (each screenshot reports its size). Take a screenshot before \
             clicking by coordinate.\n\
             Actions: left_click, right_click, double_click, triple_click (coordinate or ref; \
             `modifiers` like \"ctrl+shift\"), type (`text` into the focused element), key \
             (`text`: space-separated keys or chords like \"Enter\", \"ctrl+a\", \"Backspace\"; \
             `repeat`), hold_key (`text` held down for `duration` seconds), key_down / key_up \
             (`text` stays down across steps until released, e.g. walk while jumping), \
             left_mouse_down / left_mouse_up (at coordinate/ref or where the mouse is), screenshot (`scale` < 1 for a smaller image), wait (`duration` seconds, \
             max 10), scroll (`scroll_direction`, `scroll_amount` ticks, at `coordinate`/`ref` or \
             the center), scroll_to (ref), hover (coordinate or ref), left_click_drag \
             (`start_coordinate` → `coordinate`), zoom (`region` [x0, y0, x1, y1] of the \
             screenshot, enlarged).\n\
             JavaScript dialogs are answered automatically and reported: alerts acknowledged, \
             confirm/prompt dismissed unless `accept_dialogs` is true.\n\
             The page keeps running in real time between calls, also while you think. For \
             timing-sensitive input (games, animations) put the steps in one browser_batch, \
             where they run back to back and `wait`/`hold_key` durations are exact.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": [
                        "left_click", "right_click", "double_click", "triple_click", "type", "key",
                        "screenshot", "wait", "scroll", "scroll_to", "hover", "left_click_drag", "zoom",
                        "hold_key", "key_down", "key_up", "left_mouse_down", "left_mouse_up"
                    ]},
                    "coordinate": {"type": "array", "items": {"type": "number"}, "description": "[x, y] in the latest screenshot's pixels"},
                    "start_coordinate": {"type": "array", "items": {"type": "number"}, "description": "[x, y] where left_click_drag starts"},
                    "ref": {"type": "string", "description": "ref_N of the element to act on"},
                    "text": {"type": "string", "description": "Text to type, or keys to press"},
                    "modifiers": {"type": "string", "description": "Modifier keys held during a click, e.g. \"ctrl\", \"cmd+shift\""},
                    "scroll_direction": {"type": "string", "enum": ["up", "down", "left", "right"]},
                    "scroll_amount": {"type": "integer", "description": "Wheel ticks (default 3)"},
                    "duration": {"type": "number", "description": "Seconds to wait or hold a key (max 10)"},
                    "region": {"type": "array", "items": {"type": "number"}, "description": "[x0, y0, x1, y1] to zoom into"},
                    "repeat": {"type": "integer", "description": "Times to repeat the key sequence"},
                    "scale": {"type": "number", "description": "Image scale for screenshot/zoom, e.g. 0.5"},
                    "accept_dialogs": {"type": "boolean", "description": "Accept (OK) confirm/prompt dialogs this action raises"}
                },
                "required": ["action"]
            }),
            false,
            "Browser: {action}",
        )
    }
}

browser_tool!(BrowserComputerTool, ComputerInput, computer);

pub(crate) async fn computer(
    manager: &BrowserSessionManager,
    input: &ComputerInput,
) -> BrowserOutput {
    let mut r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    r.tab.set_accept_dialogs(input.accept_dialogs);
    let result = act(&r.tab, input).await;
    r.tab.set_accept_dialogs(false);
    match result {
        Ok(Acted { text, image }) => {
            let notes = r.notes().await;
            let mut out = r.output(text).with_notes(notes);
            if let Some(png) = image {
                out = out.with_image(&png);
            }
            out
        }
        Err(e) => {
            let notes = r.notes().await;
            let mut out = r.failure(e);
            out = out.with_notes(notes);
            out
        }
    }
}

struct Acted {
    text: String,
    image: Option<Vec<u8>>,
}

impl Acted {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            image: None,
        }
    }
}

async fn act(tab: &Tab, input: &ComputerInput) -> Result<Acted> {
    use ComputerAction::*;
    let action = input.action;
    match action {
        LeftClick | RightClick | DoubleClick | TripleClick => {
            let (button, count) = match action {
                RightClick => (Button::Right, 1),
                DoubleClick => (Button::Left, 2),
                TripleClick => (Button::Left, 3),
                _ => (Button::Left, 1),
            };
            let modifiers = parse_modifiers(input.modifiers.as_deref())?;
            let at = point(tab, input).await?;
            tab.click_point(at, button, count, modifiers).await?;
            tab.settle().await;
            Ok(Acted::text(format!(
                "{} {}",
                past_tense(action),
                target_label(input)
            )))
        }
        Hover => {
            let at = point(tab, input).await?;
            tab.hover_point(at).await?;
            Ok(Acted::text(format!("Hovered {}", target_label(input))))
        }
        Type => {
            let text = input
                .text
                .as_deref()
                .ok_or_else(|| anyhow!("type needs `text`"))?;
            tab.type_into_focused(text).await?;
            Ok(Acted::text(format!(
                "Typed {} characters",
                text.chars().count()
            )))
        }
        Key => {
            let keys = input
                .text
                .as_deref()
                .ok_or_else(|| anyhow!("key needs `text`"))?;
            let repeat = input.repeat.unwrap_or(1).clamp(1, 100);
            tab.press_keys(keys, repeat).await?;
            tab.settle().await;
            let times = if repeat > 1 {
                format!(" ×{repeat}")
            } else {
                String::new()
            };
            Ok(Acted::text(format!("Pressed {keys}{times}")))
        }
        Screenshot => {
            let shot = tab.screenshot_frame(input.scale).await?;
            Ok(Acted {
                text: format!(
                    "Screenshot of {} — {}×{} px (coordinates refer to this frame)",
                    tab.id(),
                    shot.width,
                    shot.height
                ),
                image: Some(shot.png),
            })
        }
        Zoom => {
            let region = input
                .region
                .ok_or_else(|| anyhow!("zoom needs `region` [x0, y0, x1, y1]"))?;
            let shot = tab.zoom(region, input.scale).await?;
            Ok(Acted {
                text: format!(
                    "Zoomed into {region:?}: {}×{} px (for inspection; click coordinates still refer to the last screenshot)",
                    shot.width, shot.height
                ),
                image: Some(shot.png),
            })
        }
        Wait => {
            let seconds = input.duration.unwrap_or(1.0).clamp(0.0, 10.0);
            tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
            Ok(Acted::text(format!("Waited {seconds}s")))
        }
        HoldKey => {
            let keys = input
                .text
                .as_deref()
                .ok_or_else(|| anyhow!("hold_key needs `text`"))?;
            let seconds = input.duration.unwrap_or(1.0).clamp(0.0, 10.0);
            tab.hold_keys(keys, Duration::from_secs_f64(seconds))
                .await?;
            Ok(Acted::text(format!("Held {keys} for {seconds}s")))
        }
        KeyDown => {
            let keys = input
                .text
                .as_deref()
                .ok_or_else(|| anyhow!("key_down needs `text`"))?;
            tab.key_down(keys).await?;
            Ok(Acted::text(format!("Holding {keys} (release with key_up)")))
        }
        KeyUp => {
            let keys = input
                .text
                .as_deref()
                .ok_or_else(|| anyhow!("key_up needs `text`"))?;
            tab.key_up(keys).await?;
            Ok(Acted::text(format!("Released {keys}")))
        }
        LeftMouseDown | LeftMouseUp => {
            let at = if input.coordinate.is_some() || input.r#ref.is_some() {
                Some(point(tab, input).await?)
            } else {
                None
            };
            if action == LeftMouseDown {
                tab.mouse_down(at, Button::Left).await?;
                Ok(Acted::text(format!(
                    "Holding the left button {} (release with left_mouse_up)",
                    where_label(input)
                )))
            } else {
                tab.mouse_up(at, Button::Left).await?;
                tab.settle().await;
                Ok(Acted::text(format!(
                    "Released the left button {}",
                    where_label(input)
                )))
            }
        }
        Scroll => {
            let direction = input.scroll_direction.unwrap_or(ScrollDirection::Down);
            let ticks = input.scroll_amount.unwrap_or(3).clamp(1, 50) as f64;
            let (dx, dy) = match direction {
                ScrollDirection::Up => (0.0, -ticks * TICK_PX),
                ScrollDirection::Down => (0.0, ticks * TICK_PX),
                ScrollDirection::Left => (-ticks * TICK_PX, 0.0),
                ScrollDirection::Right => (ticks * TICK_PX, 0.0),
            };
            let at = if input.coordinate.is_some() || input.r#ref.is_some() {
                point(tab, input).await?
            } else {
                let (w, h) = tab.viewport_size().await?;
                web::Point {
                    x: w / 2.0,
                    y: h / 2.0,
                }
            };
            tab.wheel(at, dx, dy).await?;
            // Let smooth scrolling land before anyone looks.
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(Acted::text(
                format!("Scrolled {direction:?} {ticks} ticks").to_lowercase(),
            ))
        }
        ScrollTo => {
            let r = input
                .r#ref
                .as_deref()
                .ok_or_else(|| anyhow!("scroll_to needs `ref`"))?;
            tab.scroll_to_ref(r).await?;
            Ok(Acted::text(format!("Scrolled {r} into view")))
        }
        LeftClickDrag => {
            let [sx, sy] = input
                .start_coordinate
                .ok_or_else(|| anyhow!("left_click_drag needs `start_coordinate`"))?;
            let [ex, ey] = input
                .coordinate
                .ok_or_else(|| anyhow!("left_click_drag needs `coordinate`"))?;
            tab.drag(tab.frame_point(sx, sy), tab.frame_point(ex, ey))
                .await?;
            tab.settle().await;
            Ok(Acted::text(format!(
                "Dragged from ({sx}, {sy}) to ({ex}, {ey})"
            )))
        }
    }
}

/// Where an action lands: the center of `ref`, or `coordinate` mapped from
/// the screenshot frame to CSS pixels.
async fn point(tab: &Tab, input: &ComputerInput) -> Result<web::Point> {
    if let Some(r) = &input.r#ref {
        return tab.ref_point(r).await;
    }
    let [x, y] = input
        .coordinate
        .ok_or_else(|| anyhow!("{:?} needs `coordinate` or `ref`", input.action))?;
    Ok(tab.frame_point(x, y))
}

fn target_label(input: &ComputerInput) -> String {
    match (&input.r#ref, input.coordinate) {
        (Some(r), _) => r.clone(),
        (None, Some([x, y])) => format!("at ({x}, {y})"),
        _ => String::new(),
    }
}

/// Like [`target_label`], but "where the mouse is" without a target.
fn where_label(input: &ComputerInput) -> String {
    match target_label(input) {
        label if label.is_empty() => "where the mouse is".to_string(),
        label => label,
    }
}

fn past_tense(action: ComputerAction) -> &'static str {
    match action {
        ComputerAction::RightClick => "Right-clicked",
        ComputerAction::DoubleClick => "Double-clicked",
        ComputerAction::TripleClick => "Triple-clicked",
        _ => "Clicked",
    }
}

/// `"ctrl+shift"` → the CDP modifier bitmask (Alt=1, Ctrl=2, Meta=4, Shift=8).
fn parse_modifiers(spec: Option<&str>) -> Result<i64> {
    let mut mask = 0;
    for part in spec
        .unwrap_or("")
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        mask |= match part.to_ascii_lowercase().as_str() {
            "alt" | "option" | "opt" => 1,
            "ctrl" | "control" => 2,
            "meta" | "cmd" | "command" | "super" | "win" | "windows" => 4,
            "shift" => 8,
            other => return Err(anyhow!("unknown modifier '{other}'")),
        };
    }
    Ok(mask)
}

#[cfg(test)]
mod tests {
    use super::super::page::{
        BrowserNavigateTool, BrowserReadPageTool, NavigateInput, ReadPageInput,
    };
    use super::super::test_support::*;
    use super::*;
    use crate::mocks::ToolTestFixture;
    use crate::tools::core::{Render, ResourcesTracker, Tool};

    fn computer(action: ComputerAction) -> ComputerInput {
        ComputerInput {
            action,
            coordinate: None,
            start_coordinate: None,
            r#ref: None,
            text: None,
            modifiers: None,
            scroll_direction: None,
            scroll_amount: None,
            duration: None,
            region: None,
            repeat: None,
            scale: None,
            accept_dialogs: false,
            target: Target::default(),
        }
    }

    fn render(out: &BrowserOutput) -> String {
        out.render(&mut ResourcesTracker::default())
    }

    #[test]
    fn modifiers_parse_to_a_bitmask() {
        assert_eq!(parse_modifiers(None).unwrap(), 0);
        assert_eq!(parse_modifiers(Some("ctrl+shift")).unwrap(), 10);
        assert_eq!(parse_modifiers(Some("Cmd")).unwrap(), 4);
        assert!(parse_modifiers(Some("hyper")).is_err());
    }

    #[tokio::test]
    async fn click_by_ref_type_and_screenshot_coordinates() -> Result<()> {
        let page = data_url(
            "<html><body style=\"margin:0\">\
             <input id=\"f\" aria-label=\"Field\" style=\"position:absolute;left:100px;top:100px;width:200px\">\
             <button id=\"b\" style=\"position:absolute;left:600px;top:400px;width:100px;height:40px\" \
               onclick=\"this.textContent='hit'\">Hit me</button>\
             <button id=\"c\" onclick=\"document.title = confirm('Sure?') ? 'yes' : 'no'\">Confirm</button>\
             </body></html>",
        );
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        BrowserNavigateTool
            .execute(
                &mut context,
                &mut NavigateInput {
                    url: page,
                    target: Target::default(),
                },
            )
            .await?;
        let mut read = ReadPageInput {
            filter: super::super::page::ReadFilter::Interactive,
            ref_id: None,
            depth: None,
            max_chars: None,
            target: Target::default(),
        };
        let tree = render(&BrowserReadPageTool.execute(&mut context, &mut read).await?);

        // Click the field by ref, type, fix it up with keys.
        let mut click = computer(ComputerAction::LeftClick);
        click.r#ref = Some(ref_on_line(&tree, "textbox \"Field\""));
        let out = BrowserComputerTool
            .execute(&mut context, &mut click)
            .await?;
        assert!(out.error.is_none(), "{:?}", out.error);
        let mut typing = computer(ComputerAction::Type);
        typing.text = Some("Grüße!".into());
        BrowserComputerTool
            .execute(&mut context, &mut typing)
            .await?;
        let mut key = computer(ComputerAction::Key);
        key.text = Some("Backspace".into());
        let out = BrowserComputerTool.execute(&mut context, &mut key).await?;
        assert_eq!(render(&out), "Pressed Backspace");

        // A half-size screenshot: its frame is 640×400, so (650, 420) in the
        // page is (325, 210) in the frame.
        let mut shot = computer(ComputerAction::Screenshot);
        shot.scale = Some(0.5);
        let out = BrowserComputerTool.execute(&mut context, &mut shot).await?;
        assert!(render(&out).contains("640×400 px"), "{}", render(&out));
        assert_eq!(out.render_images().len(), 1);
        let mut click = computer(ComputerAction::LeftClick);
        click.coordinate = Some([325.0, 210.0]);
        BrowserComputerTool
            .execute(&mut context, &mut click)
            .await?;

        let session = fixture
            .browser_sessions()
            .unwrap()
            .get_by_label("default")
            .unwrap();
        let tab = session.active_tab()?;
        assert_eq!(
            tab.javascript(
                "[document.getElementById('f').value, document.getElementById('b').textContent]"
            )
            .await?,
            r#"["Grüße","hit"]"#
        );

        // A confirm is dismissed and reported, or accepted on request.
        let mut context = fixture.context();
        let mut confirm = computer(ComputerAction::LeftClick);
        confirm.r#ref = Some(ref_on_line(&tree, "button \"Confirm\""));
        let out = BrowserComputerTool
            .execute(&mut context, &mut confirm)
            .await?;
        assert!(
            render(&out).contains("Note: A confirm dialog \"Sure?\" was dismissed."),
            "{}",
            render(&out)
        );
        confirm.accept_dialogs = true;
        let out = BrowserComputerTool
            .execute(&mut context, &mut confirm)
            .await?;
        assert!(render(&out).contains("was accepted"), "{}", render(&out));
        assert_eq!(tab.javascript("document.title").await?, "yes");
        Ok(())
    }

    #[tokio::test]
    async fn keys_are_held_for_a_duration_and_across_steps() -> Result<()> {
        let page = data_url(
            "<html><body><script>window.log = [];\
             for (const t of ['keydown', 'keyup']) {\
               document.addEventListener(t, (e) => window.log.push(t + ':' + e.code + '@' + Math.round(performance.now())));\
             }</script></body></html>",
        );
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        BrowserNavigateTool
            .execute(
                &mut context,
                &mut NavigateInput {
                    url: page,
                    target: Target::default(),
                },
            )
            .await?;

        let mut hold = computer(ComputerAction::HoldKey);
        hold.text = Some("w".into());
        hold.duration = Some(0.3);
        let out = BrowserComputerTool.execute(&mut context, &mut hold).await?;
        assert_eq!(render(&out), "Held w for 0.3s");

        for (action, keys) in [
            (ComputerAction::KeyDown, "w"),
            (ComputerAction::Key, "space"),
            (ComputerAction::KeyUp, "w"),
        ] {
            let mut step = computer(action);
            step.text = Some(keys.into());
            let out = BrowserComputerTool.execute(&mut context, &mut step).await?;
            assert!(out.error.is_none(), "{:?}", out.error);
        }

        let session = fixture
            .browser_sessions()
            .unwrap()
            .get_by_label("default")
            .unwrap();
        let log = session
            .active_tab()?
            .javascript("window.log.join(' ')")
            .await?;
        let events: Vec<&str> = log.split(' ').collect();
        let codes: Vec<&str> = events
            .iter()
            .map(|e| e.split('@').next().unwrap())
            .collect();
        assert_eq!(
            codes,
            [
                "keydown:KeyW",
                "keyup:KeyW",
                "keydown:KeyW",
                "keydown:Space",
                "keyup:Space",
                "keyup:KeyW"
            ],
            "{log}"
        );
        let at = |i: usize| events[i].split('@').nth(1).unwrap().parse::<f64>().unwrap();
        assert!(at(1) - at(0) >= 280.0, "{log}");
        Ok(())
    }

    #[tokio::test]
    async fn a_hung_page_fails_fast() -> Result<()> {
        let page = data_url(
            "<html><body><button onclick=\"setTimeout(() => { while (true) {} }, 50)\">Spin</button></body></html>",
        );
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        register_short_timeout_browser(&fixture).await?;
        let mut context = fixture.context();
        BrowserNavigateTool
            .execute(
                &mut context,
                &mut NavigateInput {
                    url: page,
                    target: Target::default(),
                },
            )
            .await?;
        let mut click = computer(ComputerAction::LeftClick);
        click.coordinate = Some([20.0, 15.0]);
        BrowserComputerTool
            .execute(&mut context, &mut click)
            .await?;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let start = std::time::Instant::now();
        let out = BrowserComputerTool
            .execute(&mut context, &mut computer(ComputerAction::Screenshot))
            .await?;
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "{:?}",
            start.elapsed()
        );
        assert!(out.error.unwrap().contains("timed out"));
        Ok(())
    }
}
