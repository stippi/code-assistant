//! A GitHub client for pull requests: reading lists, details, checks,
//! conversations and diffs, and writing comments, reviews and actions.
//!
//! Ported from T3 Code's GitHub provider. Requests go over HTTPS with the
//! token `GH_TOKEN` / `GITHUB_TOKEN` or the GitHub CLI's login supplies;
//! reads use GraphQL where GitHub offers it, REST for diffs, file contents
//! and the few writes GraphQL lacks. The client keeps inside GitHub's rate
//! limits by itself (quota reserve, pauses after refusals, ETag
//! revalidation, at most eight requests in flight).
//!
//! The network sits behind the `Transport` trait; with the `test-utils`
//! feature, `fake::FakeTransport` answers scripted requests instead.

mod auth;
mod client;
mod convert;
mod error;
mod graphql;
mod limits;
pub mod model;
mod ops;
mod repo;
pub mod transport;

#[cfg(feature = "test-utils")]
pub mod fake;

pub use auth::{GhTokenSource, StaticTokenSource, TokenSource};
pub use client::{Client, Priority};
pub use error::{
    BranchDeletionRefusal, Error, FileUnavailableReason, GraphQlError, PathSegment, Result,
};
pub use repo::{PullRequestRef, RepoRef};
