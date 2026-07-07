//! Byte-capped output buffer that keeps the head and tail of a stream.
//!
//! Long-running processes can produce unbounded output between two reads.
//! Instead of a plain ring buffer (which would drop the *start* of the
//! output — often the most interesting part, e.g. the error right after
//! launch), this buffer keeps the first half and the last half of the cap
//! and drops the middle, counting the omitted bytes.

use std::collections::VecDeque;

/// Output drained from a [`HeadTailBuffer`].
pub struct BufferedOutput {
    /// The buffered text. When bytes were dropped, a truncation marker is
    /// inserted between head and tail.
    pub text: String,
    /// Number of bytes dropped from the middle since the last take.
    pub omitted_bytes: usize,
}

pub struct HeadTailBuffer {
    head_cap: usize,
    tail_cap: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted_bytes: usize,
}

impl HeadTailBuffer {
    /// Create a buffer that retains at most `max_bytes` between two takes.
    pub fn new(max_bytes: usize) -> Self {
        let max_bytes = max_bytes.max(2);
        let head_cap = max_bytes / 2;
        Self {
            head_cap,
            tail_cap: max_bytes - head_cap,
            head: Vec::new(),
            tail: VecDeque::new(),
            omitted_bytes: 0,
        }
    }

    pub fn append(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        if self.head.len() < self.head_cap {
            let take = (self.head_cap - self.head.len()).min(rest.len());
            self.head.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        self.tail.extend(rest.iter().copied());
        while self.tail.len() > self.tail_cap {
            self.tail.pop_front();
            self.omitted_bytes += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_empty() && self.tail.is_empty()
    }

    /// Drain the buffer, resetting it for the next accumulation window.
    pub fn take(&mut self) -> BufferedOutput {
        let head = std::mem::take(&mut self.head);
        let tail: Vec<u8> = std::mem::take(&mut self.tail).into();
        let omitted_bytes = std::mem::take(&mut self.omitted_bytes);

        let mut text = String::from_utf8_lossy(&head).into_owned();
        if omitted_bytes > 0 {
            text.push_str(&format!("\n[... {omitted_bytes} bytes omitted ...]\n"));
        }
        text.push_str(&String::from_utf8_lossy(&tail));

        BufferedOutput {
            text,
            omitted_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_input_passes_through() {
        let mut buffer = HeadTailBuffer::new(1024);
        buffer.append(b"hello ");
        buffer.append(b"world");
        let out = buffer.take();
        assert_eq!(out.text, "hello world");
        assert_eq!(out.omitted_bytes, 0);
    }

    #[test]
    fn overflow_keeps_head_and_tail() {
        let mut buffer = HeadTailBuffer::new(10);
        buffer.append(b"AAAAA");
        buffer.append(b"BBBBBBBBBB"); // 15 total, cap 10 -> 5 omitted
        let out = buffer.take();
        assert_eq!(out.omitted_bytes, 5);
        assert!(out.text.starts_with("AAAAA"));
        assert!(out.text.ends_with("BBBBB"));
        assert!(out.text.contains("5 bytes omitted"));
    }

    #[test]
    fn take_resets_the_buffer() {
        let mut buffer = HeadTailBuffer::new(8);
        buffer.append(b"0123456789ABCDEF");
        let first = buffer.take();
        assert!(first.omitted_bytes > 0);
        assert!(buffer.is_empty());
        let second = buffer.take();
        assert_eq!(second.text, "");
        assert_eq!(second.omitted_bytes, 0);
    }

    #[test]
    fn invalid_utf8_at_cut_does_not_panic() {
        let mut buffer = HeadTailBuffer::new(4);
        // Multi-byte characters cut at head/tail boundaries.
        buffer.append("äöüß".as_bytes());
        let out = buffer.take();
        assert!(out.omitted_bytes > 0);
        assert!(!out.text.is_empty());
    }

    #[test]
    fn tiny_cap_is_clamped() {
        let mut buffer = HeadTailBuffer::new(0);
        buffer.append(b"xyz");
        let out = buffer.take();
        assert_eq!(out.omitted_bytes, 1);
        assert!(out.text.contains('x'));
        assert!(out.text.contains('z'));
    }
}
