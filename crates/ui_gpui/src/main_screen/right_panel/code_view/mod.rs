//! What the code views have in common: a pull request's Code tab
//! ([`pull_request`](super::pull_request)) and the Review tab
//! ([`review_view`](super::review_view)) both show changed files stacked,
//! each under its header, in one virtualized list. Everything here is
//! independent of where the diff comes from (GitHub or the local
//! repository).

pub mod anchors;
pub mod context;
pub mod file;
pub mod file_kind;
pub mod file_order;
pub mod folds;
pub mod preferences;
pub mod rows;
pub mod tree;
