use crate::{PageMetadata, WebPage, WebSearchResult};
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

const PARALLEL_BETA_HEADER_VALUE: &str = "search-extract-2025-10-10";
const DEFAULT_PAGE_SIZE: usize = 10;

pub struct ParallelClient {
    http_client: Client,
    api_key: String,
    base_url: String,
}

impl ParallelClient {
    pub fn new(api_key: String) -> Self {
        Self {
            http_client: Client::new(),
            api_key,
            base_url: "https://api.parallel.ai".to_string(),
        }
    }

    #[cfg(test)]
    pub fn with_base_url(api_key: String, base_url: String) -> Self {
        Self {
            http_client: Client::new(),
            api_key,
            base_url,
        }
    }

    pub async fn search(&self, query: &str, page: u32) -> Result<Vec<WebSearchResult>> {
        let page = page.max(1);
        let max_results = (page as usize * DEFAULT_PAGE_SIZE).max(DEFAULT_PAGE_SIZE);

        let response = self
            .http_client
            .post(format!("{}/v1beta/search", self.base_url))
            .header("Content-Type", "application/json")
            .header("x-api-key", &self.api_key)
            .header("parallel-beta", PARALLEL_BETA_HEADER_VALUE)
            .json(&json!({
                "mode": "one-shot",
                "objective": query,
                "max_results": max_results,
            }))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("Parallel search failed ({status}): {body}"));
        }

        let body: SearchResponse = response.json().await?;
        let start_index = (page as usize - 1) * DEFAULT_PAGE_SIZE;

        let mut converted = Vec::new();
        for result in body
            .results
            .into_iter()
            .skip(start_index)
            .take(DEFAULT_PAGE_SIZE)
        {
            converted.push(result.into());
        }

        Ok(converted)
    }

    pub async fn fetch(&self, url: &str) -> Result<WebPage> {
        let response = self
            .http_client
            .post(format!("{}/v1beta/extract", self.base_url))
            .header("Content-Type", "application/json")
            .header("x-api-key", &self.api_key)
            .header("parallel-beta", PARALLEL_BETA_HEADER_VALUE)
            .json(&json!({
                "urls": [url],
                "full_content": true,
                "excerpts": true,
            }))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("Parallel extract failed ({status}): {body}"));
        }

        let body: ExtractResponse = response.json().await?;
        let ExtractResponse { results, errors } = body;

        let mut fallback = None;
        for result in results {
            if result.url == url {
                return Ok(result.into());
            }

            if fallback.is_none() {
                fallback = Some(result);
            }
        }

        if let Some(result) = fallback {
            return Ok(result.into());
        }

        if let Some(error) = errors
            .into_iter()
            .find(|err| err.url.as_deref() == Some(url))
        {
            return Err(anyhow!(
                "Parallel extract failed for {}: {} ({})",
                url,
                error
                    .error_type
                    .unwrap_or_else(|| "unknown_error".to_string()),
                error.message.unwrap_or_default()
            ));
        }

        Err(anyhow!(
            "Parallel extract did not return content for {}",
            url
        ))
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    results: Vec<ParallelSearchResult>,
}

#[derive(Debug, Deserialize)]
struct ExtractResponse {
    results: Vec<ParallelExtractResult>,
    #[serde(default)]
    errors: Vec<ParallelExtractError>,
}

#[derive(Debug, Deserialize)]
struct ParallelSearchResult {
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    publish_date: Option<String>,
    #[serde(default)]
    excerpts: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ParallelExtractResult {
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    publish_date: Option<String>,
    #[serde(default)]
    excerpts: Option<Vec<String>>,
    #[serde(default)]
    full_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParallelExtractError {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl From<ParallelSearchResult> for WebSearchResult {
    fn from(result: ParallelSearchResult) -> WebSearchResult {
        let ParallelSearchResult {
            url,
            title,
            publish_date,
            excerpts,
        } = result;

        let snippet = excerpts
            .and_then(|excerpts| {
                if excerpts.is_empty() {
                    None
                } else {
                    Some(excerpts.join("\n\n"))
                }
            })
            .unwrap_or_else(|| "No excerpt available.".to_string());

        let metadata = PageMetadata {
            date: publish_date,
            ..PageMetadata::default()
        };

        let title = title.unwrap_or_else(|| url.clone());

        WebSearchResult {
            url,
            title,
            snippet,
            metadata,
        }
    }
}

impl From<ParallelExtractResult> for WebPage {
    fn from(result: ParallelExtractResult) -> WebPage {
        let ParallelExtractResult {
            url,
            title,
            publish_date,
            excerpts,
            full_content,
        } = result;

        let metadata = PageMetadata {
            date: publish_date,
            ..PageMetadata::default()
        };

        let mut content = full_content
            .or_else(|| excerpts.map(|excerpts| excerpts.join("\n\n")))
            .unwrap_or_default();

        if let Some(title) = title {
            if !title.is_empty() {
                if content.is_empty() {
                    content = title;
                } else {
                    content = format!("# {title}\n\n{content}");
                }
            }
        }

        WebPage {
            url,
            content,
            metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_search_results_with_defaults() {
        let result = ParallelSearchResult {
            url: "https://example.com".to_string(),
            title: None,
            publish_date: Some("2024-01-01".to_string()),
            excerpts: Some(vec!["First excerpt".into(), "Second excerpt".into()]),
        };

        let converted: WebSearchResult = result.into();
        assert_eq!(converted.url, "https://example.com");
        assert_eq!(converted.title, "https://example.com");
        assert!(converted.snippet.contains("First excerpt"));
        assert!(converted.metadata.date.as_deref() == Some("2024-01-01"));
    }

    #[test]
    fn converts_extract_results_with_fallback_content() {
        let result = ParallelExtractResult {
            url: "https://example.com".to_string(),
            title: None,
            publish_date: Some("2023-05-05".to_string()),
            excerpts: Some(vec!["Excerpt only".into()]),
            full_content: None,
        };

        let page: WebPage = result.into();
        assert_eq!(page.url, "https://example.com");
        assert!(page.content.contains("Excerpt only"));
        assert!(page.metadata.date.as_deref() == Some("2023-05-05"));
    }
}
