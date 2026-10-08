//! The window shown while the session store is migrated at startup, before
//! the main window opens. Only appears when there are sessions in the old
//! format; on a large store the migration takes a while.

use code_assistant_core::persistence::MigrationProgress;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme, Root};
use gpui_kit::{
    App, AppContext, Bounds, Context, Entity, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};

/// Shows the migration's progress.
pub struct MigrationView {
    progress: Option<MigrationProgress>,
}

impl MigrationView {
    fn status(&self) -> SharedString {
        match self.progress {
            None => "Starting…".into(),
            Some(progress) if progress.total == 0 => progress.phase.description().into(),
            Some(progress) => format!(
                "{} — {} of {}",
                progress.phase.description(),
                progress.done,
                progress.total
            )
            .into(),
        }
    }
}

impl Render for MigrationView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let percent = self.progress.map_or(0.0, |p| p.fraction() * 100.0);
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .gap_3()
            .px_8()
            .bg(cx.theme().background)
            .child(
                div()
                    .text_color(cx.theme().foreground)
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Updating session storage"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Sessions move to a new storage format. This happens once."),
            )
            .child(Progress::new("migration-progress").value(percent))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status()),
            )
    }
}

/// Open the migration window, follow `progress` until the sender is
/// dropped, then call `done` and close the window.
pub fn show_until_done(
    progress: async_channel::Receiver<MigrationProgress>,
    done: impl FnOnce(&mut App) + 'static,
    cx: &mut App,
) {
    let mut view: Option<Entity<MigrationView>> = None;
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(460.), px(180.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Code Assistant".into()),
                    ..Default::default()
                }),
                is_resizable: false,
                ..Default::default()
            },
            |window, cx| {
                let migration = cx.new(|_| MigrationView { progress: None });
                view = Some(migration.clone());
                cx.new(|cx| Root::new(migration, window, cx))
            },
        )
        .expect("failed to open the migration window");
    let view = view.expect("the window was built");

    cx.spawn(async move |cx| {
        while let Ok(update) = progress.recv().await {
            view.update(cx, |view, cx| {
                view.progress = Some(update);
                cx.notify();
            });
        }
        cx.update(|cx| {
            // Open the next window first: closing the last one quits.
            done(cx);
            // Removed in the same update that opened the main window, the
            // native window stayed on screen.
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            });
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_assistant_core::persistence::MigrationPhase;

    fn status(progress: Option<MigrationProgress>) -> SharedString {
        MigrationView { progress }.status()
    }

    #[test]
    fn status_names_the_phase_and_counts_sessions() {
        assert_eq!(status(None), "Starting…");
        assert_eq!(
            status(Some(MigrationProgress {
                phase: MigrationPhase::Writing,
                done: 340,
                total: 1148,
            })),
            "Writing sessions — 340 of 1148"
        );
        assert_eq!(
            status(Some(MigrationProgress {
                phase: MigrationPhase::Finishing,
                done: 0,
                total: 0,
            })),
            "Finishing up"
        );
    }
}
