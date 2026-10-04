mod ax_tree;
mod browser;
mod browser_session;
mod client;
mod page_log;
mod perplexity;
mod recording;
mod screencast;
mod tab;
#[cfg(test)]
mod tests;
pub use browser::{BrowserLaunchConfig, BrowserProfile, DEFAULT_VIEWPORT, LaunchedBrowser};
pub use browser_session::{
    BrowserSession, BrowserSessionInfo, BrowserSessionManager, DEFAULT_MAX_SESSIONS, TabInfo,
};
pub use chromiumoxide::layout::Point;
pub use client::{PageMetadata, WebClient, WebPage, WebSearchResult};
pub use perplexity::{PerplexityCitation, PerplexityClient, PerplexityMessage, PerplexityResponse};
pub use recording::{Recording, SheetFrame};
pub use screencast::{FrameMetadata, LiveFrames, ScreencastFrame};
pub use tab::{
    BrowserTimeout, BrowserTimeouts, Button, HandledDialog, MAX_SCREENSHOT_EDGE, Screenshot, Tab,
};
