mod ax_tree;
mod browser;
mod browser_session;
mod client;
mod perplexity;
mod tab;
#[cfg(test)]
mod tests;
pub use browser::{BrowserLaunchConfig, BrowserProfile, LaunchedBrowser};
pub use browser_session::{
    BrowserSession, BrowserSessionInfo, BrowserSessionManager, DEFAULT_MAX_SESSIONS, TabInfo,
};
pub use client::{PageMetadata, WebClient, WebPage, WebSearchResult};
pub use perplexity::{PerplexityCitation, PerplexityClient, PerplexityMessage, PerplexityResponse};
pub use tab::{
    BrowserTimeout, BrowserTimeouts, HandledDialog, InteractiveElement, PageObservation, Tab,
};
