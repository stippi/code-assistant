//! The "Viewed" ticks of a pull request's files, optimistic: a press shows
//! at once (an overlay over GitHub's answer), presses are sent in batches
//! (the view flushes 400 ms after the last one), one batch at a time, and
//! a press stays over the answer until a read that could have seen its
//! write arrives. Ported from T3 Code's `filesViewedStore.ts` and
//! `pullRequestFilesViewed.logic.ts`, without I/O: the view sends the
//! batches [`ViewedFiles::take_batch`] hands out and reports back.

use github::model::{FileViewedState, FilesViewed};
use std::collections::HashMap;
use std::time::SystemTime;

/// Presses sent in one write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub request: u64,
    pub files: Vec<(String, bool)>,
}

/// What the view does after a write finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Finished {
    /// Read the ticks again.
    pub refresh: bool,
    /// Tell the user the ticks could not be saved.
    pub report_failure: bool,
}

/// GitHub's answer: the states and the read they came from.
#[derive(Debug)]
struct Answer {
    states: HashMap<String, FileViewedState>,
    truncated: bool,
    read: SystemTime,
}

#[derive(Debug, Default)]
pub struct ViewedFiles {
    answer: Option<Answer>,
    /// Presses shown over the answer.
    overlay: HashMap<String, bool>,
    /// Presses not sent yet, in press order.
    queued: Vec<(String, bool)>,
    /// The request that last sent a path, while it is under way.
    sent_by: HashMap<String, u64>,
    requests: u64,
    in_flight: Option<u64>,
    /// The read that was current when a path's write succeeded: the first
    /// answer from another read could have seen the write and retires the
    /// press, whatever it says (a push in between comes back `Dismissed`).
    answered_from: HashMap<String, Option<SystemTime>>,
}

impl ViewedFiles {
    fn state(&self, path: &str) -> Option<FileViewedState> {
        self.answer.as_ref()?.states.get(path).copied()
    }

    pub fn is_viewed(&self, path: &str) -> bool {
        self.overlay
            .get(path)
            .copied()
            .unwrap_or_else(|| self.state(path) == Some(FileViewedState::Viewed))
    }

    /// Viewed once, but the file changed since ("Changed").
    pub fn is_stale(&self, path: &str) -> bool {
        !self.overlay.contains_key(path) && self.state(path) == Some(FileViewedState::Dismissed)
    }

    /// GitHub reported ticks for only some of the files.
    pub fn truncated(&self) -> bool {
        self.answer.as_ref().is_some_and(|a| a.truncated)
    }

    /// Tick or untick `path`.
    pub fn press(&mut self, path: &str, viewed: bool) {
        self.overlay.insert(path.to_owned(), viewed);
        match self.queued.iter_mut().find(|(p, _)| p == path) {
            Some(queued) => queued.1 = viewed,
            None => self.queued.push((path.to_owned(), viewed)),
        }
    }

    pub fn has_queued(&self) -> bool {
        !self.queued.is_empty()
    }

    /// The queued presses to send now; `None` when there are none or a
    /// write is still under way (writes go one after the other).
    pub fn take_batch(&mut self) -> Option<Batch> {
        if self.in_flight.is_some() || self.queued.is_empty() {
            return None;
        }
        self.requests += 1;
        let request = self.requests;
        let files = std::mem::take(&mut self.queued);
        for (path, _) in &files {
            self.sent_by.insert(path.clone(), request);
        }
        self.in_flight = Some(request);
        Some(Batch { request, files })
    }

    /// Every queued press, for a view that is going away: sent at once,
    /// whatever is under way, and not answered for.
    pub fn take_remaining(&mut self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.queued)
    }

    /// `batch` was written (`ok`) or failed.
    pub fn finish(&mut self, batch: &Batch, ok: bool) -> Finished {
        if self.in_flight == Some(batch.request) {
            self.in_flight = None;
        }
        // Paths a later batch sent again belong to that one.
        let mine: Vec<&str> = batch
            .files
            .iter()
            .map(|(path, _)| path.as_str())
            .filter(|path| self.sent_by.get(*path) == Some(&batch.request))
            .collect();
        for path in &mine {
            self.sent_by.remove(*path);
        }
        if ok {
            let read = self.answer.as_ref().map(|a| a.read);
            for path in mine {
                self.answered_from.insert(path.to_owned(), read);
            }
            return Finished {
                refresh: true,
                report_failure: false,
            };
        }
        // Put back what this batch pressed, unless pressed again since.
        let owned: Vec<&str> = mine
            .into_iter()
            .filter(|path| !self.queued.iter().any(|(p, _)| p == path))
            .collect();
        for (path, viewed) in &batch.files {
            if owned.contains(&path.as_str()) && self.overlay.get(path) == Some(viewed) {
                self.overlay.remove(path);
            }
        }
        let failed = !owned.is_empty();
        Finished {
            refresh: failed,
            report_failure: failed,
        }
    }

    /// GitHub's answer from the read at `read`; the same read handed in
    /// again changes nothing.
    pub fn answer(&mut self, files: &FilesViewed, read: SystemTime) {
        if self.answer.as_ref().is_some_and(|a| a.read == read) {
            return;
        }
        self.answer = Some(Answer {
            states: files
                .files
                .iter()
                .map(|f| (f.path.clone(), f.state))
                .collect(),
            truncated: files.truncated,
            read,
        });
        let pending = |path: &str| {
            self.queued.iter().any(|(p, _)| p == path) || self.sent_by.contains_key(path)
        };
        let answered: Vec<String> = self
            .answered_from
            .iter()
            .filter(|(path, from)| !pending(path) && **from != Some(read))
            .map(|(path, _)| path.clone())
            .collect();
        for path in &answered {
            self.answered_from.remove(path);
        }
        let settled: Vec<String> = self
            .overlay
            .iter()
            .filter(|(path, pressed)| {
                !pending(path)
                    && (answered.contains(path)
                        || (self.state(path) == Some(FileViewedState::Viewed)) == **pressed)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in settled {
            self.overlay.remove(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use github::model::FileViewed;
    use std::time::Duration;

    fn read(n: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(n)
    }

    fn files(state: FileViewedState) -> FilesViewed {
        FilesViewed {
            files: vec![FileViewed {
                path: "a.ts".into(),
                state,
            }],
            truncated: false,
        }
    }

    fn unviewed() -> ViewedFiles {
        let mut viewed = ViewedFiles::default();
        viewed.answer(&files(FileViewedState::Unviewed), read(1));
        viewed
    }

    #[test]
    fn states_come_from_the_answer_under_the_presses() {
        let mut viewed = ViewedFiles::default();
        viewed.answer(
            &FilesViewed {
                files: vec![
                    FileViewed {
                        path: "a".into(),
                        state: FileViewedState::Viewed,
                    },
                    FileViewed {
                        path: "b".into(),
                        state: FileViewedState::Dismissed,
                    },
                ],
                truncated: true,
            },
            read(1),
        );
        assert!(viewed.is_viewed("a") && !viewed.is_viewed("b") && !viewed.is_viewed("c"));
        assert!(viewed.is_stale("b") && !viewed.is_stale("a"));
        assert!(viewed.truncated());
        viewed.press("b", true);
        assert!(viewed.is_viewed("b") && !viewed.is_stale("b"));
    }

    #[test]
    fn presses_batch_and_hold_until_a_read_that_could_have_seen_them() {
        let mut viewed = unviewed();
        viewed.press("a.ts", true);
        viewed.press("b.ts", true);
        viewed.press("a.ts", false);
        viewed.press("a.ts", true);
        assert!(viewed.is_viewed("a.ts"));
        let batch = viewed.take_batch().expect("queued");
        assert_eq!(
            batch.files,
            [("a.ts".to_owned(), true), ("b.ts".to_owned(), true)]
        );
        assert_eq!(viewed.take_batch(), None, "nothing more queued");

        let done = viewed.finish(&batch, true);
        assert_eq!(
            done,
            Finished {
                refresh: true,
                report_failure: false
            }
        );
        // The read from before the write, handed in again, is no answer.
        viewed.answer(&files(FileViewedState::Unviewed), read(1));
        assert!(viewed.is_viewed("a.ts"));
        // A push landed between the write and the next read: GitHub wins.
        viewed.answer(&files(FileViewedState::Dismissed), read(2));
        assert!(!viewed.is_viewed("a.ts") && viewed.is_stale("a.ts"));
    }

    #[test]
    fn writes_go_one_after_the_other() {
        let mut viewed = unviewed();
        viewed.press("a.ts", true);
        let first = viewed.take_batch().unwrap();
        viewed.press("b.ts", true);
        assert_eq!(viewed.take_batch(), None, "waits for the first write");
        viewed.finish(&first, true);
        let second = viewed.take_batch().expect("now it goes");
        assert_eq!(second.files, [("b.ts".to_owned(), true)]);
        assert_eq!(second.request, first.request + 1);
    }

    #[test]
    fn a_failed_press_is_put_back_and_reported() {
        let mut viewed = unviewed();
        viewed.press("a.ts", true);
        let batch = viewed.take_batch().unwrap();
        assert_eq!(
            viewed.finish(&batch, false),
            Finished {
                refresh: true,
                report_failure: true
            }
        );
        assert!(!viewed.is_viewed("a.ts"));
    }

    #[test]
    fn a_failure_leaves_presses_made_since_alone() {
        let mut viewed = unviewed();
        viewed.press("a.ts", true);
        let batch = viewed.take_batch().unwrap();
        viewed.press("a.ts", false);
        assert_eq!(viewed.finish(&batch, false), Finished::default());
        assert!(!viewed.is_viewed("a.ts"));
        assert!(viewed.has_queued());
    }

    #[test]
    fn a_queued_press_holds_over_an_answer_that_cannot_have_seen_it() {
        let mut viewed = unviewed();
        viewed.press("a.ts", true);
        viewed.answer(&files(FileViewedState::Viewed), read(2));
        assert!(viewed.is_viewed("a.ts"));
        let batch = viewed.take_batch().unwrap();
        viewed.finish(&batch, true);
        viewed.answer(&files(FileViewedState::Viewed), read(2));
        assert!(viewed.is_viewed("a.ts"), "the same read is no new answer");
        viewed.answer(&files(FileViewedState::Unviewed), read(3));
        assert!(!viewed.is_viewed("a.ts"));
    }

    #[test]
    fn an_agreeing_answer_settles_a_press_without_a_write() {
        let mut viewed = unviewed();
        viewed.press("a.ts", true);
        viewed.take_remaining();
        viewed.answer(&files(FileViewedState::Viewed), read(2));
        assert!(viewed.is_viewed("a.ts"));
        viewed.answer(&files(FileViewedState::Unviewed), read(3));
        assert!(
            !viewed.is_viewed("a.ts"),
            "the press settled with the first"
        );
    }

    #[test]
    fn leaving_takes_everything_queued() {
        let mut viewed = unviewed();
        viewed.press("a.ts", true);
        let _under_way = viewed.take_batch().unwrap();
        viewed.press("b.ts", true);
        assert_eq!(viewed.take_remaining(), [("b.ts".to_owned(), true)]);
        assert!(!viewed.has_queued());
    }
}
