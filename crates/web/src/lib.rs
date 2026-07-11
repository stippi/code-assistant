mod browser;
mod browser_session;
mod client;
mod perplexity;
#[cfg(test)]
mod tests;
pub use browser::{BrowserLaunchConfig, BrowserProfile, LaunchedBrowser};
pub use browser_session::{
    BrowserSession, BrowserSessionInfo, BrowserSessionManager, PageObservation,
    DEFAULT_MAX_SESSIONS,
};
pub use client::{PageMetadata, WebClient, WebPage, WebSearchResult};
pub use perplexity::{PerplexityCitation, PerplexityClient, PerplexityMessage, PerplexityResponse};
