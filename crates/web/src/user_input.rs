//! Input from a person at a browser panel, forwarded to the page as is.
//!
//! The agent's verbs ([`Tab::click_point`], [`Tab::press_keys`], …) are
//! shaped for a model: whole clicks, named chords. A panel forwards raw
//! events instead, as the user's mouse and keyboard produce them.

use crate::tab::{Button, KeyExtras, Tab, key_definition};
use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchMouseEventParams, DispatchMouseEventType, InsertTextParams, MouseButton,
};
use chromiumoxide::layout::Point;

/// One input event from a browser panel. Points are page CSS pixels;
/// `modifiers` is the CDP bitmask (Alt=1, Ctrl=2, Meta=4, Shift=8),
/// `buttons` the mouse buttons held after the event (Left=1, Right=2,
/// Middle=4).
#[derive(Debug, Clone, PartialEq)]
pub enum UserInput {
    MouseMove {
        at: Point,
        buttons: i64,
        modifiers: i64,
    },
    MouseDown {
        at: Point,
        button: Button,
        click_count: u32,
        buttons: i64,
        modifiers: i64,
    },
    MouseUp {
        at: Point,
        button: Button,
        click_count: u32,
        buttons: i64,
        modifiers: i64,
    },
    Wheel {
        at: Point,
        dx: f64,
        dy: f64,
        modifiers: i64,
    },
    /// `key` names the key as `browser_computer` does (`"a"`, `"1"`,
    /// `"Enter"`, `"ArrowLeft"`); `text` is what it types, if anything.
    KeyDown {
        key: String,
        text: Option<String>,
        modifiers: i64,
        commands: Vec<String>,
    },
    KeyUp {
        key: String,
        modifiers: i64,
    },
    /// Text from an input method or the clipboard.
    InsertText(String),
    Navigate(String),
    /// Back (`-1`) or forward (`1`) in the tab's history.
    History(i32),
    Reload,
}

impl Tab {
    /// Forward one event from a browser panel.
    pub async fn user_input(&self, input: UserInput) -> Result<()> {
        let limit = self.timeouts().command;
        match input {
            UserInput::MouseMove {
                at,
                buttons,
                modifiers,
            } => {
                let button = held_button(buttons);
                self.bounded("mouse move", limit, async {
                    self.user_mouse(
                        DispatchMouseEventType::MouseMoved,
                        at,
                        button,
                        0,
                        buttons,
                        modifiers,
                    )
                    .await
                })
                .await
            }
            UserInput::MouseDown {
                at,
                button,
                click_count,
                buttons,
                modifiers,
            } => {
                self.bounded("mouse press", limit, async {
                    self.user_mouse(
                        DispatchMouseEventType::MousePressed,
                        at,
                        Some(button),
                        click_count,
                        buttons,
                        modifiers,
                    )
                    .await
                })
                .await
            }
            UserInput::MouseUp {
                at,
                button,
                click_count,
                buttons,
                modifiers,
            } => {
                self.bounded("mouse release", limit, async {
                    self.user_mouse(
                        DispatchMouseEventType::MouseReleased,
                        at,
                        Some(button),
                        click_count,
                        buttons,
                        modifiers,
                    )
                    .await
                })
                .await
            }
            UserInput::Wheel {
                at,
                dx,
                dy,
                modifiers,
            } => {
                self.bounded("scroll", limit, async {
                    let event = DispatchMouseEventParams::builder()
                        .r#type(DispatchMouseEventType::MouseWheel)
                        .x(at.x)
                        .y(at.y)
                        .delta_x(dx)
                        .delta_y(dy)
                        .modifiers(modifiers)
                        .build()
                        .map_err(anyhow::Error::msg)?;
                    self.page().execute(event).await?;
                    Ok(())
                })
                .await
            }
            UserInput::KeyDown {
                key,
                text,
                modifiers,
                commands,
            } => {
                let Some(def) = key_definition(&key) else {
                    // A key the US table does not know: its text is all the
                    // page can get.
                    if let Some(text) = text {
                        return self.insert_text(text).await;
                    }
                    anyhow::bail!("unknown key '{key}'");
                };
                let extras = KeyExtras {
                    typed: text,
                    commands,
                };
                self.bounded(
                    "key press",
                    limit,
                    self.key_event(def, modifiers, true, extras),
                )
                .await
            }
            UserInput::KeyUp { key, modifiers } => {
                let Some(def) = key_definition(&key) else {
                    return Ok(());
                };
                self.bounded(
                    "key release",
                    limit,
                    self.key_event(def, modifiers, false, KeyExtras::default()),
                )
                .await
            }
            UserInput::InsertText(text) => self.insert_text(text).await,
            UserInput::Navigate(url) => self.navigate(&url).await,
            UserInput::History(delta) => self.history(delta).await,
            UserInput::Reload => {
                self.bounded("reload", self.timeouts().navigation, async {
                    self.page().reload().await?;
                    Ok(())
                })
                .await
            }
        }
    }

    async fn insert_text(&self, text: String) -> Result<()> {
        self.bounded("typing", self.timeouts().command, async {
            self.page().execute(InsertTextParams::new(text)).await?;
            Ok(())
        })
        .await
    }

    async fn user_mouse(
        &self,
        kind: DispatchMouseEventType,
        at: Point,
        button: Option<Button>,
        click_count: u32,
        buttons: i64,
        modifiers: i64,
    ) -> Result<()> {
        let mut event = DispatchMouseEventParams::builder()
            .r#type(kind)
            .x(at.x)
            .y(at.y)
            .modifiers(modifiers)
            .buttons(buttons);
        if let Some(button) = button {
            event = event.button(match button {
                Button::Left => MouseButton::Left,
                Button::Right => MouseButton::Right,
                Button::Middle => MouseButton::Middle,
            });
        }
        if click_count > 0 {
            event = event.click_count(click_count as i64);
        }
        self.page()
            .execute(event.build().map_err(anyhow::Error::msg)?)
            .await?;
        Ok(())
    }
}

/// The button a move drags with: the first one held.
fn held_button(buttons: i64) -> Option<Button> {
    [(1, Button::Left), (2, Button::Right), (4, Button::Middle)]
        .into_iter()
        .find(|(mask, _)| buttons & mask != 0)
        .map(|(_, button)| button)
}
