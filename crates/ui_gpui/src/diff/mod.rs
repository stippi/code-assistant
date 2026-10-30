//! Diffs as the Review view and the chat's diff cards show them.
//!
//! - [`model`]: rows ([`model::DiffLine`]) in hunks, from a line diff of two
//!   texts or from a parsed patch, with word-level emphasis
//! - [`chunks`]: hunks cut into the items of a virtualized list
//! - [`syntax`]: tree-sitter highlighting of both sides
//! - [`prepare`]: [`prepare::PreparedFileDiff`], all of the above for one
//!   file, built off the UI thread
//! - [`render`] and [`rows`]: rows as one painted element, with a gutter of
//!   new or old and new line numbers

pub mod chunks;
pub mod model;
pub mod prepare;
pub mod render;
pub mod rows;
pub mod syntax;
