//! Turning icons that show something is in progress.
//!
//! The rotation runs on gpui's timed transformations: the icon is painted
//! once, and the window turns it in the scene it already has before each
//! frame. The view holding the spinner is not re-rendered for that, unlike
//! with `with_animation`, which re-renders it every frame. With reduced
//! motion the icon stands still.

use std::time::Duration;

use gpui_kit::{SharedString, Svg, TimedTransformation, Transformation, svg};

/// One full turn of a spinner.
pub const SPIN_PERIOD: Duration = Duration::from_secs(2);

/// The icon at `path`, turning once per [`SPIN_PERIOD`]. Size and color are
/// the caller's, like on any `svg()`.
pub fn spinner(path: impl Into<SharedString>) -> Svg {
    spinner_with_period(path, SPIN_PERIOD)
}

/// The icon at `path`, turning once per `period`.
pub fn spinner_with_period(path: impl Into<SharedString>, period: Duration) -> Svg {
    svg()
        .path(path)
        .with_timed_transformation(TimedTransformation::rotation(
            period,
            Transformation::default(),
        ))
}
