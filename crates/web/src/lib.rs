mod client;
mod parallel;
mod perplexity;
#[cfg(test)]
mod tests;
pub use client::{PageMetadata, WebClient, WebPage, WebSearchResult};
pub use parallel::ParallelClient;
pub use perplexity::{PerplexityCitation, PerplexityClient, PerplexityMessage, PerplexityResponse};
