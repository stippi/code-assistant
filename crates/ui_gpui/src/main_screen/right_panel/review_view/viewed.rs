//! The Review tab's "Viewed" ticks of one repository, local: a tick keeps
//! what the file was then ([`ViewedFile`]). The listing's fingerprint
//! decides at once while it is the same; once it moved (a touch, a
//! commit, another compare mode) the file's content decides when its diff
//! is read: the same content keeps the tick, another one shows the file
//! as changed since it was viewed.

use crate::main_screen::right_panel::code_view::list::ViewedMark;
use crate::shared::reviewed_files_store::{ViewedFile, ViewedFiles};

/// A listed file's tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Unviewed,
    Viewed,
    /// Ticked for another fingerprint; its content is not known yet.
    Unconfirmed,
    /// Ticked, and it changed since.
    Changed,
}

impl Tick {
    pub fn mark(self) -> ViewedMark {
        ViewedMark {
            viewed: matches!(self, Tick::Viewed | Tick::Unconfirmed),
            stale: self == Tick::Changed,
        }
    }
}

#[derive(Debug, Default)]
pub struct Ticks {
    files: ViewedFiles,
}

impl Ticks {
    pub fn new(files: ViewedFiles) -> Self {
        Self { files }
    }

    pub fn files(&self) -> &ViewedFiles {
        &self.files
    }

    /// The tick of `path`, listed with `fingerprint`; `content` once its
    /// diff is read.
    pub fn tick(&self, path: &str, fingerprint: Option<&str>, content: Option<&str>) -> Tick {
        let Some(viewed) = self.files.get(path) else {
            return Tick::Unviewed;
        };
        if viewed.fingerprint.is_some() && viewed.fingerprint.as_deref() == fingerprint {
            return Tick::Viewed;
        }
        match (content, viewed.content.as_deref()) {
            (Some(now), Some(then)) if now == then => Tick::Viewed,
            (Some(_), _) => Tick::Changed,
            (None, _) => Tick::Unconfirmed,
        }
    }

    /// Tick or untick `path` as it is now.
    pub fn set(
        &mut self,
        path: &str,
        viewed: bool,
        fingerprint: Option<&str>,
        content: Option<&str>,
    ) {
        match viewed {
            true => {
                self.files.insert(
                    path.to_owned(),
                    ViewedFile {
                        fingerprint: fingerprint.map(str::to_owned),
                        content: content.map(str::to_owned),
                    },
                );
            }
            false => {
                self.files.remove(path);
            }
        }
    }

    /// The diff of `path` was read: a tick it confirms takes the current
    /// fingerprint (and the content, where it had none). `true` when the
    /// ticks changed and want saving.
    pub fn confirm(
        &mut self,
        path: &str,
        fingerprint: Option<&str>,
        content: Option<&str>,
    ) -> bool {
        if self.tick(path, fingerprint, content) != Tick::Viewed {
            return false;
        }
        let Some(viewed) = self.files.get_mut(path) else {
            return false;
        };
        let updated = ViewedFile {
            fingerprint: fingerprint.map(str::to_owned),
            content: content
                .map(str::to_owned)
                .or_else(|| viewed.content.clone()),
        };
        let changed = *viewed != updated;
        *viewed = updated;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tick_holds_while_the_fingerprint_or_else_the_content_is_the_same() {
        let mut ticks = Ticks::default();
        assert_eq!(ticks.tick("a", Some("f1"), None), Tick::Unviewed);
        ticks.set("a", true, Some("f1"), Some("c1"));
        assert_eq!(ticks.tick("a", Some("f1"), None), Tick::Viewed);
        // Touched: unknown until the content is read.
        assert_eq!(ticks.tick("a", Some("f2"), None), Tick::Unconfirmed);
        assert_eq!(ticks.tick("a", Some("f2"), Some("c1")), Tick::Viewed);
        assert_eq!(ticks.tick("a", Some("f2"), Some("c2")), Tick::Changed);
        ticks.set("a", false, Some("f2"), Some("c2"));
        assert_eq!(ticks.tick("a", Some("f2"), Some("c2")), Tick::Unviewed);
    }

    #[test]
    fn a_tick_without_a_fingerprint_needs_the_content() {
        let mut ticks = Ticks::default();
        ticks.set("a", true, None, None);
        assert_eq!(ticks.tick("a", None, None), Tick::Unconfirmed);
        // Ticked before its content was known: any content changed it.
        assert_eq!(ticks.tick("a", None, Some("c")), Tick::Changed);
    }

    #[test]
    fn a_confirmed_tick_takes_the_new_fingerprint_and_the_content() {
        let mut ticks = Ticks::default();
        ticks.set("a", true, Some("f1"), None);
        assert!(ticks.confirm("a", Some("f1"), Some("c1")));
        assert!(!ticks.confirm("a", Some("f1"), Some("c1")));
        assert_eq!(ticks.tick("a", Some("f2"), Some("c1")), Tick::Viewed);
        assert!(ticks.confirm("a", Some("f2"), Some("c1")));
        assert_eq!(ticks.tick("a", Some("f2"), None), Tick::Viewed);
        // A changed file keeps its old tick until it is ticked again.
        assert!(!ticks.confirm("a", Some("f3"), Some("c2")));
        assert_eq!(ticks.tick("a", Some("f3"), Some("c2")), Tick::Changed);
    }

    #[test]
    fn marks_show_unconfirmed_ticks_as_viewed() {
        assert!(Tick::Unconfirmed.mark().viewed);
        assert_eq!(
            Tick::Changed.mark(),
            ViewedMark {
                viewed: false,
                stale: true
            }
        );
    }
}
