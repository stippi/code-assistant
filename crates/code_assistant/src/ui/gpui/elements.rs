//! Backward-compatibility re-exports.
//!
//! The actual implementation now lives in `blocks/mod.rs`. This thin shim
//! keeps existing `use crate::ui::gpui::elements::*` paths working during
//! the incremental migration.

pub use super::blocks::*;
