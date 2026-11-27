//! What the code views have in common: a pull request's Code tab
//! ([`pull_request`](super::pull_request)) and the Review tab
//! ([`review_view`](super::review_view)) both show changed files stacked,
//! each under its header (kept on top while its rows scroll), in one
//! virtualized list, with the same diff options, folds, "Viewed" ticks,
//! file tree and comment box. Everything here is independent of where the
//! diff comes from (GitHub or the local repository); a view holds a
//! [`list::CodeList`] and implements [`list::CodeHost`] for the rest.

pub mod anchors;
pub mod aside;
pub mod body;
pub mod context;
pub mod draft;
pub mod file;
pub mod file_kind;
pub mod file_order;
pub mod folds;
pub mod header;
pub mod list;
pub mod options;
pub mod preferences;
pub mod rows;
pub mod tree;
