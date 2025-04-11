mod client;
mod perplexity;
#[cfg(test)]
mod tests;
pub use client::{WebClient, WebPage, WebSearchResult};
pub use perplexity::{PerplexityClient, PerplexityMessage, PerplexityResponse, PerplexityCitation};
