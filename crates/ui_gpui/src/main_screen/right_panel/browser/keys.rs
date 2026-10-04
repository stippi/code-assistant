//! GPUI keystrokes as key events for the page.
//!
//! The page gets the key by the name `browser_computer` uses (GPUI's names —
//! `enter`, `left`, `pageup`, `f5` — are among the aliases the web crate
//! resolves) plus the text it types. Headless Chrome on macOS does not turn
//! ⌘/⌥ shortcuts into editing commands itself, so those carry the command
//! by name. Copy, cut and paste go through the system clipboard instead.

use gpui_kit::{Keystroke, Modifiers};

#[derive(Debug, PartialEq)]
pub enum KeyAction {
    /// Forward to the page.
    Send {
        key: String,
        text: Option<String>,
        commands: Vec<String>,
    },
    Copy,
    Cut,
    Paste,
    /// An app shortcut, not for the page.
    Pass,
}

/// The CDP modifier bitmask (Alt=1, Ctrl=2, Meta=4, Shift=8).
pub fn modifier_mask(m: &Modifiers) -> i64 {
    (m.alt as i64) | (m.control as i64) << 1 | (m.platform as i64) << 2 | (m.shift as i64) << 3
}

/// What a key press in the panel does.
pub fn key_down(ks: &Keystroke) -> KeyAction {
    let m = &ks.modifiers;
    let key = ks.key.as_str();
    if m.secondary() && !m.alt {
        match key {
            "c" => return KeyAction::Copy,
            "x" => return KeyAction::Cut,
            "v" => return KeyAction::Paste,
            _ => {}
        }
    }
    let commands: Vec<String> = mac_command(ks)
        .map(|c| vec![c.to_string()])
        .unwrap_or_default();
    if m.platform && commands.is_empty() {
        return KeyAction::Pass;
    }
    // Named keys (`enter`, `left`, …) type what the key table says; a
    // character key types the character the layout produced.
    let text = match key {
        "space" => Some(" ".to_string()),
        _ if key.chars().count() > 1 => None,
        _ => ks
            .key_char
            .clone()
            .filter(|c| !c.chars().any(char::is_control)),
    };
    KeyAction::Send {
        key: key.to_string(),
        text: text.filter(|_| !m.control && !m.platform),
        commands,
    }
}

/// The editing command a macOS shortcut stands for.
fn mac_command(ks: &Keystroke) -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let m = &ks.modifiers;
    let select = m.shift;
    let command = match (m.platform, m.alt, ks.key.as_str()) {
        (true, false, "a") => "selectAll",
        (true, false, "z") if select => "redo",
        (true, false, "z") => "undo",
        (true, false, "left") if select => "moveToBeginningOfLineAndModifySelection",
        (true, false, "left") => "moveToBeginningOfLine",
        (true, false, "right") if select => "moveToEndOfLineAndModifySelection",
        (true, false, "right") => "moveToEndOfLine",
        (true, false, "up") if select => "moveToBeginningOfDocumentAndModifySelection",
        (true, false, "up") => "moveToBeginningOfDocument",
        (true, false, "down") if select => "moveToEndOfDocumentAndModifySelection",
        (true, false, "down") => "moveToEndOfDocument",
        (true, false, "backspace") => "deleteToBeginningOfLine",
        (false, true, "left") if select => "moveWordLeftAndModifySelection",
        (false, true, "left") => "moveWordLeft",
        (false, true, "right") if select => "moveWordRightAndModifySelection",
        (false, true, "right") => "moveWordRight",
        (false, true, "backspace") => "deleteWordBackward",
        (false, true, "delete") => "deleteWordForward",
        _ => return None,
    };
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(spec: &str, key_char: Option<&str>) -> Keystroke {
        let mut ks = Keystroke::parse(spec).unwrap();
        ks.key_char = key_char.map(str::to_string);
        ks
    }

    fn send(key: &str, text: Option<&str>, commands: &[&str]) -> KeyAction {
        KeyAction::Send {
            key: key.into(),
            text: text.map(str::to_string),
            commands: commands.iter().map(|c| c.to_string()).collect(),
        }
    }

    #[test]
    fn characters_type_what_the_layout_produced() {
        assert_eq!(key_down(&ks("a", Some("a"))), send("a", Some("a"), &[]));
        assert_eq!(
            key_down(&ks("shift-1", Some("!"))),
            send("1", Some("!"), &[])
        );
        assert_eq!(key_down(&ks("alt-s", Some("ß"))), send("s", Some("ß"), &[]));
        assert_eq!(
            key_down(&ks("space", Some(" "))),
            send("space", Some(" "), &[])
        );
    }

    #[test]
    fn named_keys_and_control_chords_type_nothing_themselves() {
        assert_eq!(key_down(&ks("enter", Some("\r"))), send("enter", None, &[]));
        assert_eq!(
            key_down(&ks("backspace", None)),
            send("backspace", None, &[])
        );
        assert_eq!(key_down(&ks("ctrl-a", Some("a"))), send("a", None, &[]));
    }

    #[test]
    fn clipboard_shortcuts_are_the_panels() {
        let secondary = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        for (key, action) in [
            ("c", KeyAction::Copy),
            ("x", KeyAction::Cut),
            ("v", KeyAction::Paste),
        ] {
            assert_eq!(key_down(&ks(&format!("{secondary}-{key}"), None)), action);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_shortcuts_carry_their_editing_command() {
        assert_eq!(
            key_down(&ks("cmd-a", None)),
            send("a", None, &["selectAll"])
        );
        assert_eq!(
            key_down(&ks("cmd-shift-z", None)),
            send("z", None, &["redo"])
        );
        assert_eq!(
            key_down(&ks("alt-shift-left", None)),
            send("left", None, &["moveWordLeftAndModifySelection"])
        );
        assert_eq!(
            key_down(&ks("cmd-w", None)),
            KeyAction::Pass,
            "app shortcut"
        );
    }

    #[test]
    fn modifiers_map_to_the_cdp_mask() {
        let m = Keystroke::parse("ctrl-shift-a").unwrap().modifiers;
        assert_eq!(modifier_mask(&m), 2 | 8);
        let m = Keystroke::parse("alt-cmd-a").unwrap().modifiers;
        assert_eq!(modifier_mask(&m), 1 | 4);
    }
}
