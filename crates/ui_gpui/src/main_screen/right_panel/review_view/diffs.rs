//! The diffs of the shown repository, read one file at a time: the next
//! is the one nearest the view that has none for its listing entry (a
//! folded file only while its tick waits for its content). Both sides are
//! read, diffed and highlighted off the UI thread.

use super::viewed::Tick;
use super::{ReviewFile, ReviewView};
use crate::main_screen::right_panel::code_view::file::LoadedFile;
use crate::main_screen::right_panel::code_view::list::{self as code_list, CodeHost};
use crate::main_screen::right_panel::code_view::rows::files_by_proximity;
use crate::main_screen::right_panel::pull_request::data::session_service;
use crate::shared::reviewed_files_store::content_token;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{AppContext, Context};

impl ReviewView {
    /// Whether `file`'s diff is to be read.
    fn wants_diff(&self, file: &ReviewFile) -> bool {
        !file.current()
            && (!self.list.folds.is_folded(&file.listed.path)
                || self.tick_of(file) == Tick::Unconfirmed)
    }

    /// Read the next diff wanted, unless one is under way.
    pub(super) fn ensure_diff_request(&mut self, cx: &mut Context<Self>) {
        if self.in_flight.is_some() {
            return;
        }
        let (Some(session_id), Some(root), Some(service)) = (
            self.session_id.clone(),
            self.repo.clone(),
            session_service(cx),
        ) else {
            return;
        };
        code_list::sync_rows(self);
        let top = self.list.list_state.logical_scroll_top().item_ix;
        // The rows may lag behind the files; listing order covers the rest.
        let next = files_by_proximity(&self.list.rows, self.shown.len(), top)
            .into_iter()
            .filter_map(|ix| self.shown.get(ix).copied())
            .chain(0..self.files.len())
            .find(|&f| self.files.get(f).is_some_and(|file| self.wants_diff(file)));
        let Some(next) = next else {
            return;
        };
        let listed = self.files[next].listed.clone();
        let base = self.repo_data().and_then(|data| data.base.clone());
        let (mode, scope) = (self.mode, self.scope);
        let theme = cx.theme().highlight_theme.clone();
        let view = self.list.diff_view();
        let path = listed.path.clone();
        let task = cx.spawn(async move |this, cx| {
            let content = service
                .get_review_file_diff(session_id, root, mode, base, listed.clone())
                .await;
            let read = match content {
                Ok(content) => Ok(cx
                    .background_spawn(async move {
                        let token = content_token(&content);
                        let file = LoadedFile::from_content(&listed, &content, &theme, view);
                        (listed, file, token)
                    })
                    .await),
                Err(error) => Err((listed, error)),
            };
            this.update(cx, |this, cx| this.diff_read(scope, read, cx))
                .ok();
        });
        self.in_flight = Some((path, task));
        cx.notify();
    }

    pub(super) fn diff_read(
        &mut self,
        scope: u64,
        read: Result<
            (git::ChangedFile, LoadedFile, Option<String>),
            (git::ChangedFile, anyhow::Error),
        >,
        cx: &mut Context<Self>,
    ) {
        self.in_flight = None;
        if scope != self.scope {
            return self.ensure_diff_request(cx);
        }
        let (listed, mut file, token) = match read {
            Ok(read) => read,
            Err((listed, error)) => {
                if let Some(gpui) = cx.try_global::<crate::Gpui>() {
                    gpui.display_error(format!("Failed to load diff: {error:#}"));
                }
                // Read, if empty: the next file goes on.
                let mut empty = LoadedFile::pending(
                    listed.path.clone(),
                    listed.orig_path.clone(),
                    listed.status,
                );
                empty.loaded = true;
                (listed, empty, None)
            }
        };
        let Some(ix) = self.files.iter().position(|f| f.listed == listed) else {
            // The listing moved on meanwhile.
            return self.ensure_diff_request(cx);
        };
        // Shaped for the options then; they may have changed meanwhile.
        file.reshape(self.list.diff_view());
        file.stamp = self.list.stamp();
        let entry = &mut self.files[ix];
        entry.file = file;
        entry.read_for = Some(listed.clone());
        entry.content = token.clone();
        if self.ticks.confirm(
            &listed.path,
            listed.fingerprint.as_deref(),
            token.as_deref(),
        ) {
            self.save_ticks(cx);
        }
        self.place_annotations(cx);
        self.ensure_diff_request(cx);
    }
}
