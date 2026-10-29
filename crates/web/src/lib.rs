mod ax_tree;
mod bridge;
mod browser;
mod browser_session;
mod client;
mod page_log;
mod perplexity;
mod profile_lock;
mod recording;
mod screencast;
mod tab;
#[cfg(test)]
mod tests;
mod user_input;
pub use bridge::Binding;
pub use browser::{BrowserLaunchConfig, BrowserProfile, DEFAULT_VIEWPORT, LaunchedBrowser};
pub use browser_session::{
    BrowserSession, BrowserSessionInfo, BrowserSessionManager, DEFAULT_MAX_SESSIONS, TabInfo,
    ViewGuard,
};
pub use chromiumoxide::layout::Point;
pub use client::{PageMetadata, SearchBlocked, WebClient, WebPage, WebSearchResult};
pub use perplexity::{PerplexityCitation, PerplexityClient, PerplexityMessage, PerplexityResponse};
pub use profile_lock::ProfileInUse;
pub use recording::{Recording, SheetFrame};
pub use screencast::{FrameMetadata, LiveFrames, ScreencastFrame};
pub use tab::{
    BrowserTimeout, BrowserTimeouts, Button, HandledDialog, MAX_SCREENSHOT_EDGE, Screenshot, Tab,
};
pub use user_input::UserInput;
