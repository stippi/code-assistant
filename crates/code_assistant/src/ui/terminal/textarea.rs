// Adapted from codex-rs/tui/src/bottom_pane/textarea.rs (Apache 2.0 licensed)
// Custom textarea widget for the terminal UI.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::WidgetRef;
use std::cell::Ref;
use std::cell::RefCell;
use std::ops::Range;
use textwrap::Options;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

fn is_word_separator(ch: char) -> bool {
    WORD_SEPARATORS.contains(ch)
}

/// On Windows, AltGr sends ALT+CONTROL together. Detect this to avoid
/// treating AltGr characters as control combos.
#[cfg(windows)]
#[inline]
fn is_altgr(mods: KeyModifiers) -> bool {
    mods.contains(KeyModifiers::ALT) && mods.contains(KeyModifiers::CONTROL)
}

#[cfg(not(windows))]
#[inline]
fn is_altgr(_mods: KeyModifiers) -> bool {
    false
}

/// An atomic inline element (e.g. paste placeholder or image indicator).
/// The cursor cannot be placed inside an element; it jumps over it as a unit.
#[derive(Debug, Clone)]
struct TextElement {
    range: Range<usize>,
}

#[derive(Debug)]
pub struct TextArea {
    text: String,
    cursor_pos: usize,
    wrap_cache: RefCell<Option<WrapCache>>,
    preferred_col: Option<usize>,
    kill_buffer: String,
    elements: Vec<TextElement>,
}

#[derive(Debug, Clone)]
struct WrapCache {
    width: u16,
    lines: Vec<Range<usize>>,
}

impl TextArea {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
            wrap_cache: RefCell::new(None),
            preferred_col: None,
            kill_buffer: String::new(),
            elements: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        self.kill_buffer.clear();
        self.elements.clear();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert_str(&mut self, text: &str) {
        self.insert_str_at(self.cursor_pos, text);
    }

    pub fn insert_str_at(&mut self, pos: usize, text: &str) {
        let pos = self.clamp_pos_for_insertion(pos);
        self.text.insert_str(pos, text);
        self.wrap_cache.replace(None);
        if pos <= self.cursor_pos {
            self.cursor_pos += text.len();
        }
        self.shift_elements(pos, 0, text.len());
        self.preferred_col = None;
    }

    /// Insert an atomic element at the cursor. The element text is inserted into the
    /// buffer but treated as a single unit for cursor movement and deletion.
    pub fn insert_element(&mut self, text: &str) {
        let start = self.clamp_pos_for_insertion(self.cursor_pos);
        // Insert raw text
        self.text.insert_str(start, text);
        self.wrap_cache.replace(None);
        let end = start + text.len();
        // Shift existing elements
        self.shift_elements(start, 0, text.len());
        // Register the new element
        self.elements.push(TextElement { range: start..end });
        self.elements.sort_by_key(|e| e.range.start);
        // Place cursor after element
        self.cursor_pos = end;
        self.preferred_col = None;
    }

    /// Returns true if the textarea has any elements (paste placeholders, image indicators).
    pub fn has_elements(&self) -> bool {
        !self.elements.is_empty()
    }

    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        let range = self.expand_range_to_element_boundaries(range);
        let start = range.start.clamp(0, self.text.len());
        let end = range.end.clamp(0, self.text.len());
        if start > end {
            return;
        }
        let removed_len = end - start;
        let inserted_len = text.len();
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(start..end, text);
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        self.update_elements_after_replace(start, end, inserted_len);

        self.cursor_pos = if self.cursor_pos < start {
            self.cursor_pos
        } else if self.cursor_pos <= end {
            start + inserted_len
        } else {
            ((self.cursor_pos as isize) + diff) as usize
        }
        .min(self.text.len());

        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
    }

    pub fn cursor(&self) -> usize {
        self.cursor_pos
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(pos.clamp(0, self.text.len()));
        self.preferred_col = None;
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        if width == 0 {
            return 1;
        }
        self.wrapped_lines(width).len().max(1) as u16
    }

    /// Compute the on-screen cursor position.
    pub fn cursor_position(&self, area: Rect) -> Option<(u16, u16)> {
        if area.width == 0 {
            return Some((area.x, area.y));
        }
        let lines = self.wrapped_lines(area.width);
        let i = Self::wrapped_line_index_by_start(&lines, self.cursor_pos)?;
        let ls = &lines[i];
        let col = self.text[ls.start..self.cursor_pos].width() as u16;
        Some((area.x + col, area.y + i as u16))
    }

    pub fn input(&mut self, event: KeyEvent) {
        match event {
            // C0 control character fallbacks (terminals that don't report CONTROL modifier)
            KeyEvent {
                code: KeyCode::Char('\u{0002}'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_cursor_left();
            }
            KeyEvent {
                code: KeyCode::Char('\u{0006}'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_cursor_right();
            }
            KeyEvent {
                code: KeyCode::Char('\u{0010}'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_cursor_up();
            }
            KeyEvent {
                code: KeyCode::Char('\u{000e}'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_cursor_down();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
                ..
            } => self.insert_str(&c.to_string()),
            KeyEvent {
                code: KeyCode::Char('j' | 'm'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.insert_str("\n"),
            KeyEvent {
                code: KeyCode::Char('h'),
                modifiers,
                ..
            } if modifiers == (KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.delete_backward_word()
            }
            // Windows AltGr: treat as plain character
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if is_altgr(modifiers) => self.insert_str(&c.to_string()),
            KeyEvent {
                code: KeyCode::Backspace,
                modifiers: KeyModifiers::ALT,
                ..
            } => self.delete_backward_word(),
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.delete_backward(1),
            KeyEvent {
                code: KeyCode::Delete,
                modifiers: KeyModifiers::ALT,
                ..
            } => self.delete_forward_word(),
            KeyEvent {
                code: KeyCode::Delete,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.delete_forward(1),
            KeyEvent {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.delete_backward_word();
            }
            // Meta-b / Meta-f for word navigation
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: KeyModifiers::ALT,
                ..
            } => {
                self.set_cursor(self.beginning_of_previous_word());
            }
            KeyEvent {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::ALT,
                ..
            } => {
                self.set_cursor(self.end_of_next_word());
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.kill_to_beginning_of_line();
            }
            KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.kill_to_end_of_line();
            }
            KeyEvent {
                code: KeyCode::Char('y'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.yank();
            }
            // Cursor movement
            KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.move_cursor_left(),
            KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.move_cursor_right(),
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.move_cursor_left(),
            KeyEvent {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.move_cursor_right(),
            KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.move_cursor_up(),
            KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.move_cursor_down(),
            // Alt+Arrow / Ctrl+Arrow for word navigation
            KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::ALT | KeyModifiers::CONTROL,
                ..
            } => {
                self.set_cursor(self.beginning_of_previous_word());
            }
            KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::ALT | KeyModifiers::CONTROL,
                ..
            } => {
                self.set_cursor(self.end_of_next_word());
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => self.move_cursor_up(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => self.move_cursor_down(),
            KeyEvent {
                code: KeyCode::Home,
                ..
            } => self.move_cursor_to_beginning_of_line(),
            KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.move_cursor_to_beginning_of_line(),
            KeyEvent {
                code: KeyCode::End, ..
            } => self.move_cursor_to_end_of_line(),
            KeyEvent {
                code: KeyCode::Char('e'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.move_cursor_to_end_of_line(),
            _ => {}
        }
    }

    // ####### Input Functions #######

    pub fn delete_backward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos == 0 {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.prev_atomic_boundary(target);
            if target == 0 {
                break;
            }
        }
        self.replace_range(target..self.cursor_pos, "");
    }

    pub fn delete_forward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos >= self.text.len() {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.next_atomic_boundary(target);
            if target >= self.text.len() {
                break;
            }
        }
        self.replace_range(self.cursor_pos..target, "");
    }

    pub fn delete_backward_word(&mut self) {
        let start = self.beginning_of_previous_word();
        self.kill_range(start..self.cursor_pos);
    }

    pub fn delete_forward_word(&mut self) {
        let end = self.end_of_next_word();
        if end > self.cursor_pos {
            self.kill_range(self.cursor_pos..end);
        }
    }

    pub fn kill_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        let range = if self.cursor_pos == eol {
            if eol < self.text.len() {
                Some(self.cursor_pos..eol + 1)
            } else {
                None
            }
        } else {
            Some(self.cursor_pos..eol)
        };
        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    pub fn kill_to_beginning_of_line(&mut self) {
        let bol = self.beginning_of_current_line();
        let range = if self.cursor_pos == bol {
            if bol > 0 {
                Some(bol - 1..bol)
            } else {
                None
            }
        } else {
            Some(bol..self.cursor_pos)
        };
        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    pub fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        let text = self.kill_buffer.clone();
        self.insert_str(&text);
    }

    fn kill_range(&mut self, range: Range<usize>) {
        let range = self.expand_range_to_element_boundaries(range);
        if range.start >= range.end {
            return;
        }
        let removed = self.text[range.clone()].to_string();
        if removed.is_empty() {
            return;
        }
        self.kill_buffer = removed;
        self.replace_range(range, "");
    }

    // ####### Cursor Movement #######

    pub fn move_cursor_left(&mut self) {
        self.cursor_pos = self.prev_atomic_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor_pos = self.next_atomic_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn move_cursor_up(&mut self) {
        if let Some((target_col, maybe_line)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::wrapped_line_index_by_start(lines, self.cursor_pos) {
                    let cur_range = &lines[idx];
                    let target_col = self
                        .preferred_col
                        .unwrap_or_else(|| self.text[cur_range.start..self.cursor_pos].width());
                    if idx > 0 {
                        let prev = &lines[idx - 1];
                        Some((target_col, Some((prev.start, prev.end.saturating_sub(1)))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match maybe_line {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    self.cursor_pos = 0;
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // Fallback to logical line navigation
        if let Some(prev_nl) = self.text[..self.cursor_pos].rfind('\n') {
            let target_col = match self.preferred_col {
                Some(c) => c,
                None => {
                    let c = self.current_display_col();
                    self.preferred_col = Some(c);
                    c
                }
            };
            let prev_line_start = self.text[..prev_nl].rfind('\n').map(|i| i + 1).unwrap_or(0);
            self.move_to_display_col_on_line(prev_line_start, prev_nl, target_col);
        } else {
            self.cursor_pos = 0;
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_down(&mut self) {
        if let Some((target_col, move_to_last)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::wrapped_line_index_by_start(lines, self.cursor_pos) {
                    let cur_range = &lines[idx];
                    let target_col = self
                        .preferred_col
                        .unwrap_or_else(|| self.text[cur_range.start..self.cursor_pos].width());
                    if idx + 1 < lines.len() {
                        let next = &lines[idx + 1];
                        Some((target_col, Some((next.start, next.end.saturating_sub(1)))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match move_to_last {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    self.cursor_pos = self.text.len();
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // Fallback to logical line navigation
        let target_col = match self.preferred_col {
            Some(c) => c,
            None => {
                let c = self.current_display_col();
                self.preferred_col = Some(c);
                c
            }
        };
        if let Some(next_nl) = self.text[self.cursor_pos..]
            .find('\n')
            .map(|i| i + self.cursor_pos)
        {
            let next_line_start = next_nl + 1;
            let next_line_end = self.text[next_line_start..]
                .find('\n')
                .map(|i| i + next_line_start)
                .unwrap_or(self.text.len());
            self.move_to_display_col_on_line(next_line_start, next_line_end, target_col);
        } else {
            self.cursor_pos = self.text.len();
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_to_beginning_of_line(&mut self) {
        let bol = self.beginning_of_current_line();
        self.set_cursor(bol);
        self.preferred_col = None;
    }

    pub fn move_cursor_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        self.set_cursor(eol);
    }

    // ####### Word Navigation #######

    fn beginning_of_previous_word(&self) -> usize {
        let prefix = &self.text[..self.cursor_pos];
        let Some((first_non_ws_idx, ch)) = prefix
            .char_indices()
            .rev()
            .find(|&(_, ch)| !ch.is_whitespace())
        else {
            return 0;
        };
        let is_separator = is_word_separator(ch);
        let mut start = first_non_ws_idx;
        for (idx, ch) in prefix[..first_non_ws_idx].char_indices().rev() {
            if ch.is_whitespace() || is_word_separator(ch) != is_separator {
                start = idx + ch.len_utf8();
                break;
            }
            start = idx;
        }
        self.adjust_pos_out_of_elements(start, true)
    }

    fn end_of_next_word(&self) -> usize {
        let Some(first_non_ws) = self.text[self.cursor_pos..].find(|c: char| !c.is_whitespace())
        else {
            return self.text.len();
        };
        let word_start = self.cursor_pos + first_non_ws;
        let mut iter = self.text[word_start..].char_indices();
        let Some((_, first_ch)) = iter.next() else {
            return word_start;
        };
        let is_separator = is_word_separator(first_ch);
        let mut end = self.text.len();
        for (idx, ch) in iter {
            if ch.is_whitespace() || is_word_separator(ch) != is_separator {
                end = word_start + idx;
                break;
            }
        }
        self.adjust_pos_out_of_elements(end, false)
    }

    // ####### Internal Helpers #######

    fn current_display_col(&self) -> usize {
        let bol = self.beginning_of_current_line();
        self.text[bol..self.cursor_pos].width()
    }

    fn wrapped_line_index_by_start(lines: &[Range<usize>], pos: usize) -> Option<usize> {
        let idx = lines.partition_point(|r| r.start <= pos);
        if idx == 0 {
            None
        } else {
            Some(idx - 1)
        }
    }

    fn move_to_display_col_on_line(
        &mut self,
        line_start: usize,
        line_end: usize,
        target_col: usize,
    ) {
        let mut width_so_far = 0usize;
        for (i, g) in self.text[line_start..line_end].grapheme_indices(true) {
            width_so_far += g.width();
            if width_so_far > target_col {
                self.cursor_pos = line_start + i;
                self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
                return;
            }
        }
        self.cursor_pos = line_end;
    }

    fn beginning_of_line(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn beginning_of_current_line(&self) -> usize {
        self.beginning_of_line(self.cursor_pos)
    }

    fn end_of_line(&self, pos: usize) -> usize {
        self.text[pos..]
            .find('\n')
            .map(|i| i + pos)
            .unwrap_or(self.text.len())
    }

    fn end_of_current_line(&self) -> usize {
        self.end_of_line(self.cursor_pos)
    }

    fn clamp_pos_to_char_boundary(&self, pos: usize) -> usize {
        let pos = pos.min(self.text.len());
        if self.text.is_char_boundary(pos) {
            return pos;
        }
        let mut prev = pos;
        while prev > 0 && !self.text.is_char_boundary(prev) {
            prev -= 1;
        }
        prev
    }

    /// Like prev_grapheme_boundary but jumps over elements atomically.
    fn prev_atomic_boundary(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        // If at or inside an element, jump to its start.
        if let Some(idx) = self
            .elements
            .iter()
            .position(|e| pos > e.range.start && pos <= e.range.end)
        {
            return self.elements[idx].range.start;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.prev_boundary(&self.text, 0) {
            Ok(Some(b)) => {
                // If we landed inside an element, jump to its start.
                if let Some(idx) = self.find_element_containing(b) {
                    self.elements[idx].range.start
                } else {
                    b
                }
            }
            Ok(None) => 0,
            Err(_) => pos.saturating_sub(1),
        }
    }

    /// Like next_grapheme_boundary but jumps over elements atomically.
    fn next_atomic_boundary(&self, pos: usize) -> usize {
        if pos >= self.text.len() {
            return self.text.len();
        }
        // If at start or inside an element, jump to its end.
        if let Some(idx) = self
            .elements
            .iter()
            .position(|e| pos >= e.range.start && pos < e.range.end)
        {
            return self.elements[idx].range.end;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.next_boundary(&self.text, 0) {
            Ok(Some(b)) => {
                // If we landed inside an element, jump to its end.
                if let Some(idx) = self.find_element_containing(b) {
                    self.elements[idx].range.end
                } else {
                    b
                }
            }
            Ok(None) => self.text.len(),
            Err(_) => pos.saturating_add(1),
        }
    }

    #[expect(clippy::unwrap_used)]
    fn wrapped_lines(&self, width: u16) -> Ref<'_, Vec<Range<usize>>> {
        {
            let mut cache = self.wrap_cache.borrow_mut();
            let needs_recalc = match cache.as_ref() {
                Some(c) => c.width != width,
                None => true,
            };
            if needs_recalc {
                let lines = wrap_ranges(
                    &self.text,
                    Options::new(width as usize).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
                );
                *cache = Some(WrapCache { width, lines });
            }
        }

        let cache = self.wrap_cache.borrow();
        Ref::map(cache, |c| &c.as_ref().unwrap().lines)
    }

    // ===== Element support =====

    fn find_element_containing(&self, pos: usize) -> Option<usize> {
        self.elements
            .iter()
            .position(|e| pos > e.range.start && pos < e.range.end)
    }

    /// Clamp position to the nearest element boundary if it falls inside one.
    fn clamp_pos_to_nearest_boundary(&self, pos: usize) -> usize {
        let pos = self.clamp_pos_to_char_boundary(pos);
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    /// Clamp position for insertion: never insert into the middle of an element.
    fn clamp_pos_for_insertion(&self, pos: usize) -> usize {
        let pos = self.clamp_pos_to_char_boundary(pos).min(self.text.len());
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    /// If `pos` falls inside an element, snap to start or end.
    fn adjust_pos_out_of_elements(&self, pos: usize, prefer_start: bool) -> usize {
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            if prefer_start {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    /// Expand a range to include any intersecting elements fully.
    fn expand_range_to_element_boundaries(&self, mut range: Range<usize>) -> Range<usize> {
        loop {
            let mut changed = false;
            for e in &self.elements {
                if e.range.start < range.end && e.range.end > range.start {
                    let new_start = range.start.min(e.range.start);
                    let new_end = range.end.max(e.range.end);
                    if new_start != range.start || new_end != range.end {
                        range = new_start..new_end;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        range
    }

    fn shift_elements(&mut self, at: usize, removed: usize, inserted: usize) {
        let end = at + removed;
        let diff = inserted as isize - removed as isize;
        // Remove elements fully deleted by the operation
        self.elements
            .retain(|e| !(e.range.start >= at && e.range.end <= end));
        for e in &mut self.elements {
            if e.range.end <= at {
                // before edit - no change
            } else if e.range.start >= end {
                // after edit - shift
                e.range.start = ((e.range.start as isize) + diff) as usize;
                e.range.end = ((e.range.end as isize) + diff) as usize;
            } else {
                // Overlap: snap to new bounds
                let new_start = at.min(e.range.start);
                let new_end = at + inserted.max(e.range.end.saturating_sub(end));
                e.range.start = new_start;
                e.range.end = new_end;
            }
        }
    }

    fn update_elements_after_replace(&mut self, start: usize, end: usize, inserted_len: usize) {
        self.shift_elements(start, end.saturating_sub(start), inserted_len);
    }
}

/// Style used for highlighted element placeholders in the textarea.
fn element_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

impl WidgetRef for &TextArea {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.wrapped_lines(area.width);
        for (row, idx) in (0..lines.len()).enumerate() {
            if row as u16 >= area.height {
                break;
            }
            let r = &lines[idx];
            let y = area.y + row as u16;
            let line_range = r.start..r.end.saturating_sub(1);
            if let Some(text_slice) = self.text.get(line_range.clone()) {
                // Draw the base line with default style.
                buf.set_string(area.x, y, text_slice, Style::default());

                // Overlay styled segments for elements that intersect this line.
                for elem in &self.elements {
                    let overlap_start = elem.range.start.max(line_range.start);
                    let overlap_end = elem.range.end.min(line_range.end);
                    if overlap_start >= overlap_end {
                        continue;
                    }
                    if let Some(elem_slice) = self.text.get(overlap_start..overlap_end) {
                        let col_offset = self.text[line_range.start..overlap_start].width() as u16;
                        buf.set_string(area.x + col_offset, y, elem_slice, element_style());
                    }
                }
            }
        }
    }
}

/// Compute byte ranges of wrapped lines using textwrap.
/// Each range includes trailing whitespace and a sentinel +1 byte (matching codex convention).
fn wrap_ranges<'a, O>(text: &str, width_or_options: O) -> Vec<Range<usize>>
where
    O: Into<Options<'a>>,
{
    let opts = width_or_options.into();
    let mut lines: Vec<Range<usize>> = Vec::new();
    for line in textwrap::wrap(text, opts).iter() {
        match line {
            std::borrow::Cow::Borrowed(slice) => {
                let start = unsafe { slice.as_ptr().offset_from(text.as_ptr()) as usize };
                let end = start + slice.len();
                let trailing_spaces = text[end..].chars().take_while(|c| *c == ' ').count();
                lines.push(start..end + trailing_spaces + 1);
            }
            std::borrow::Cow::Owned(_) => {
                // textwrap may produce owned strings for certain edge cases;
                // fall back to simple char-based ranges
                let start = if let Some(prev) = lines.last() {
                    prev.end
                } else {
                    0
                };
                let end = (start + line.len()).min(text.len());
                lines.push(start..end + 1);
            }
        }
    }
    // Ensure at least one line for empty text
    if lines.is_empty() {
        lines.push(0..1);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_insert_and_text() {
        let mut ta = TextArea::new();
        ta.insert_str("hello");
        assert_eq!(ta.text(), "hello");
        assert_eq!(ta.cursor(), 5);
    }

    #[test]
    fn test_clear() {
        let mut ta = TextArea::new();
        ta.insert_str("hello");
        ta.clear();
        assert_eq!(ta.text(), "");
        assert_eq!(ta.cursor(), 0);
    }

    #[test]
    fn test_delete_backward() {
        let mut ta = TextArea::new();
        ta.insert_str("hello");
        ta.delete_backward(1);
        assert_eq!(ta.text(), "hell");
        assert_eq!(ta.cursor(), 4);
    }

    #[test]
    fn test_delete_forward() {
        let mut ta = TextArea::new();
        ta.insert_str("hello");
        ta.set_cursor(0);
        ta.delete_forward(1);
        assert_eq!(ta.text(), "ello");
        assert_eq!(ta.cursor(), 0);
    }

    #[test]
    fn test_newline_insert() {
        let mut ta = TextArea::new();
        ta.insert_str("line1");
        ta.insert_str("\n");
        ta.insert_str("line2");
        assert_eq!(ta.text(), "line1\nline2");
    }

    #[test]
    fn test_desired_height() {
        let mut ta = TextArea::new();
        ta.insert_str("short");
        assert_eq!(ta.desired_height(80), 1);

        ta.clear();
        ta.insert_str("line1\nline2\nline3");
        assert_eq!(ta.desired_height(80), 3);
    }

    #[test]
    fn test_cursor_position() {
        let mut ta = TextArea::new();
        ta.insert_str("hello");
        let area = Rect::new(0, 0, 80, 5);
        let pos = ta.cursor_position(area);
        assert_eq!(pos, Some((5, 0)));
    }

    #[test]
    fn test_empty_desired_height() {
        let ta = TextArea::new();
        assert_eq!(ta.desired_height(80), 1);
    }

    #[test]
    fn test_kill_and_yank() {
        let mut ta = TextArea::new();
        ta.insert_str("hello world");
        ta.kill_to_end_of_line();
        // cursor is at end, so nothing to kill unless at end of line with more lines
        assert_eq!(ta.text(), "hello world"); // no change since cursor at end

        ta.set_cursor(5);
        ta.kill_to_end_of_line();
        assert_eq!(ta.text(), "hello");

        ta.set_cursor(5);
        ta.yank();
        assert_eq!(ta.text(), "hello world");
    }

    #[test]
    fn test_insert_element() {
        let mut ta = TextArea::new();
        ta.insert_str("before ");
        ta.insert_element("[Pasted 100 chars]");
        ta.insert_str(" after");
        assert_eq!(ta.text(), "before [Pasted 100 chars] after");
        // Cursor should be after the element + " after"
        assert_eq!(ta.cursor(), ta.text().len());
        // There should be one element
        assert_eq!(ta.elements.len(), 1);
        assert_eq!(ta.elements[0].range, 7..25);
    }

    #[test]
    fn test_element_cursor_skips() {
        let mut ta = TextArea::new();
        ta.insert_str("a");
        ta.insert_element("[IMG]");
        ta.insert_str("b");
        // Text: "a[IMG]b", cursor at end (7)
        assert_eq!(ta.text(), "a[IMG]b");
        assert_eq!(ta.cursor(), 7);

        // Move left: should skip from 'b' over element to 'a'
        ta.move_cursor_left(); // to end of element = 6 -> actually to start of 'b' which is 6
                               // move_cursor_left goes to prev_atomic_boundary(7) = 6 ('b' start)
                               // actually 'b' is at pos 6, so prev_atomic from 7 is 6
                               // then move_cursor_left again from 6 should jump over element to 1
        ta.move_cursor_left(); // from 6 to element start = 1
        assert_eq!(ta.cursor(), 1);
        ta.move_cursor_left(); // from 1 to 0
        assert_eq!(ta.cursor(), 0);
    }

    #[test]
    fn test_delete_backward_removes_element() {
        let mut ta = TextArea::new();
        ta.insert_str("a");
        ta.insert_element("[IMG]");
        ta.insert_str("b");
        // "a[IMG]b", cursor at 7
        ta.set_cursor(6); // right after element
        ta.delete_backward(1);
        // Should delete entire element
        assert_eq!(ta.text(), "ab");
        assert_eq!(ta.elements.len(), 0);
    }

    #[test]
    fn test_clear_removes_elements() {
        let mut ta = TextArea::new();
        ta.insert_element("[Pasted 50 chars]");
        ta.insert_element("[Image 1]");
        assert_eq!(ta.elements.len(), 2);
        ta.clear();
        assert_eq!(ta.elements.len(), 0);
        assert_eq!(ta.text(), "");
    }
}
