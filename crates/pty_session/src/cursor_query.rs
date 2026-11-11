//! Answering cursor position queries (`ESC [ 6 n`) on behalf of a terminal.
//!
//! A program on a PTY may ask the terminal where the cursor is and wait for
//! the reply (`ESC [ row ; col R`). Normally the terminal emulator on the
//! other end answers; our sessions have none — output goes to the model and
//! to display-only terminal cards that never write back — so the program
//! would wait forever.
//!
//! On Windows this hits every command: `portable-pty` creates the
//! pseudoconsole with `PSEUDOCONSOLE_INHERIT_CURSOR`, so ConPTY itself sends
//! the query on startup and blocks the child until it gets an answer.

/// The query: Device Status Report, "report cursor position".
const QUERY: &[u8] = b"\x1b[6n";

/// The answer we give: top-left. Nothing tracks the real position, and
/// output always starts on a fresh screen.
pub(crate) const ANSWER: &[u8] = b"\x1b[1;1R";

/// Finds queries in a stream of output chunks, also when one is split
/// across chunks.
#[derive(Default)]
pub(crate) struct CursorQueryScanner {
    /// How many bytes of `QUERY` the previous chunk ended with.
    matched: usize,
}

impl CursorQueryScanner {
    /// The number of queries completed by `chunk`.
    pub(crate) fn scan(&mut self, chunk: &[u8]) -> usize {
        let mut found = 0;
        for &byte in chunk {
            if byte == QUERY[self.matched] {
                self.matched += 1;
            } else {
                // ESC occurs only at the start of the query, so a mismatch
                // can only begin a new match there.
                self.matched = usize::from(byte == QUERY[0]);
            }
            if self.matched == QUERY.len() {
                found += 1;
                self.matched = 0;
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_query_in_one_chunk() {
        let mut scanner = CursorQueryScanner::default();
        assert_eq!(scanner.scan(b"hello\x1b[6nworld"), 1);
        assert_eq!(scanner.scan(b"plain output"), 0);
    }

    #[test]
    fn finds_several_queries() {
        let mut scanner = CursorQueryScanner::default();
        assert_eq!(scanner.scan(b"\x1b[6n\x1b[6nx\x1b[6n"), 3);
    }

    #[test]
    fn finds_a_query_split_across_chunks() {
        for split in 1..QUERY.len() {
            let mut scanner = CursorQueryScanner::default();
            let (head, tail) = QUERY.split_at(split);
            let first = [b"abc".as_slice(), head].concat();
            let second = [tail, b"def".as_slice()].concat();
            assert_eq!(scanner.scan(&first), 0, "split at {split}");
            assert_eq!(scanner.scan(&second), 1, "split at {split}");
        }
    }

    #[test]
    fn finds_a_query_spread_over_single_bytes() {
        let mut scanner = CursorQueryScanner::default();
        let found: usize = QUERY.iter().map(|b| scanner.scan(&[*b])).sum();
        assert_eq!(found, 1);
    }

    #[test]
    fn ignores_other_sequences() {
        let mut scanner = CursorQueryScanner::default();
        // Similar but different: DSR status (5n), other CSI sequences, a
        // broken-off prefix followed by a fresh query.
        assert_eq!(scanner.scan(b"\x1b[5n\x1b[61n\x1b[31m\x1b[6"), 0);
        assert_eq!(scanner.scan(b"m"), 0);
        assert_eq!(scanner.scan(b"\x1b\x1b[6n"), 1);
    }
}
