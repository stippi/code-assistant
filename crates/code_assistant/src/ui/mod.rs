pub mod backend;
pub mod gpui;
pub mod terminal;

// The UI trait layer (UserInterface, UiEvent, DisplayFragment, streaming)
// lives in the domain crate; re-exported so the frontends keep using
// `crate::ui::…` paths.
#[allow(unused_imports)]
pub use code_assistant_core::ui::*;
