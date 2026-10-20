//! A living architecture overview of a session's project: its components
//! (packages, modules, files), what each is responsible for, what it
//! exposes, and which components depend on which.
//!
//! Two halves keep the token cost low:
//! - [`scan`] builds the component tree and the dependency edges
//!   deterministically, from manifests, folders, public items and imports
//!   ([`extract`]), for Rust, TypeScript/JavaScript and Python;
//! - the model only explains: one call per component, fed a compact digest
//!   of it ([`digest`]), and only when that digest has no cached summary
//!   ([`analyzer`]). Body-only edits leave digests (and summaries) alone.
//!
//! Frontends drive it through `SessionService::architecture_overview` and
//! `SessionService::architecture_summary`. Settings live in
//! `architecture.json` ([`ArchitectureConfig`]). See
//! `docs/architecture-panel.md`.

pub mod analyzer;
pub mod config;
pub mod digest;
pub mod extract;
pub mod scan;

pub use analyzer::{
    Analyzer, ArchitectureOverview, CachedSummary, Completion, LlmSummarizer, Summarizer,
};
pub use config::ArchitectureConfig;
pub use digest::{ComponentSummary, InterfaceNote};
pub use extract::Language;
pub use scan::{ArchitectureModel, Component, ComponentKind, Edge, is_relevant_path};
