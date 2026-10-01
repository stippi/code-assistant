mod browser;
mod browser_session;
mod client;
mod perplexity;
#[cfg(test)]
mod tests;
pub use browser::{BrowserLaunchConfig, BrowserProfile, LaunchedBrowser};
pub use browser_session::{
    BrowserSession, BrowserSessionInfo, BrowserSessionManager, BrowserTimeout, BrowserTimeouts,
    DEFAULT_MAX_SESSIONS, HandledDialog, InteractiveElement, PageObservation,
};
pub use client::{PageMetadata, WebClient, WebPage, WebSearchResult};
pub use perplexity::{PerplexityCitation, PerplexityClient, PerplexityMessage, PerplexityResponse};
