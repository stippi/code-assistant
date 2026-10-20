use crate::browser::{BrowserLaunchConfig, LaunchedBrowser};
use anyhow::Result;
use htmd::{Element, HtmlToMarkdown};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use url::Url;

pub struct WebClient {
    http_client: Client,
    browser: LaunchedBrowser,
}

impl WebClient {
    pub async fn new() -> Result<Self> {
        // Ephemeral, headless profile: the old behavior, now expressed through
        // the shared launch path (see `browser::LaunchedBrowser`).
        let browser = LaunchedBrowser::launch(BrowserLaunchConfig::default()).await?;

        Ok(Self {
            http_client: Client::new(),
            browser,
        })
    }

    pub async fn search(&self, query: &str, page: u32) -> Result<Vec<WebSearchResult>> {
        let search_url = format!(
            "https://html.duckduckgo.com/html/?q={}&s={}",
            utf8_percent_encode(query, NON_ALPHANUMERIC),
            (page - 1) * 20
        );

        let resp = self.http_client.get(&search_url).send().await?;
        let status = resp.status().as_u16();
        let html = resp.text().await?;
        parse_search_results(status, &html)
    }

    pub async fn fetch(&self, url: &str) -> Result<WebPage> {
        let url = Url::parse(url)?;
        let page = self.browser.browser.new_page(url.as_str()).await?;

        // Wait for page to load
        let page = page.wait_for_navigation().await?;

        // Get content either from main content or body
        let html = if let Ok(main) = page.find_element("main, article, #content, .content").await {
            main.inner_html().await?.unwrap_or_default()
        } else {
            let body = page.find_element("body").await?;
            body.inner_html().await?.unwrap_or_default()
        };

        // Convert HTML to Markdown with improved handlers
        let converter = HtmlToMarkdown::builder()
            .skip_tags(vec!["script", "style", "noscript"])
            .add_handler(vec!["svg"], |_: Element| Some("".to_string()))
            // .add_handler(vec!["img"], |elem: Element| {
            //     elem.attrs.
            //     let alt = elem.attr("alt").unwrap_or("Image");
            //     if !alt.is_empty() {
            //         Some(format!("[{}]", alt))
            //     } else {
            //         Some("".to_string())
            //     }
            // })
            .build();
        let content = converter.convert(&html).unwrap();

        // Clean up the markdown
        let image_pattern = Regex::new(r"!\[.*?\]\([^)]*\)\n?").unwrap();
        let empty_heading_pattern = Regex::new(r"\n*#+ *\n+").unwrap();
        let relative_link_pattern = Regex::new(r"\[([^\]]+)\]\(/[^)]+\)").unwrap();
        let multiple_newlines = Regex::new(r"\n{3,}").unwrap();
        let empty_brackets = Regex::new(r"\[\]").unwrap();

        // Apply cleanup
        let mut content = image_pattern.replace_all(&content, "").to_string();
        content = empty_heading_pattern.replace_all(&content, "").to_string();
        content = multiple_newlines.replace_all(&content, "\n\n").to_string();
        content = empty_brackets.replace_all(&content, "").to_string();

        // Handle relative links (preserve links as in original code)
        let base_url = url.origin().ascii_serialization();
        content = relative_link_pattern
            .replace_all(&content, |caps: &regex::Captures| {
                let link_text = &caps[1];
                let link_url = &caps[0][caps[1].len() + 3..].trim_end_matches(')');
                format!("[{link_text}]({base_url}{link_url})")
            })
            .into_owned();

        Ok(WebPage {
            url: url.to_string(),
            content,
            metadata: PageMetadata::default(),
        })
    }
}

/// DuckDuckGo answered with its bot check instead of results: it does that
/// to repeated requests for a while (with HTTP 202). Not a missing result,
/// and not something to retry right away.
#[derive(Debug)]
pub struct SearchBlocked;

impl std::fmt::Display for SearchBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "DuckDuckGo blocked the search with a bot check (too many requests); \
             try again later",
        )
    }
}

impl std::error::Error for SearchBlocked {}

/// The results on a DuckDuckGo HTML result page that came with HTTP
/// `status`. Fails with [`SearchBlocked`] on the bot check, and on any
/// other unsuccessful status, rather than reading those as no results.
fn parse_search_results(status: u16, html: &str) -> Result<Vec<WebSearchResult>> {
    let document = Html::parse_document(html);
    let challenge = Selector::parse("#challenge-form, .anomaly-modal__modal").unwrap();
    if status == 202 || document.select(&challenge).next().is_some() {
        return Err(SearchBlocked.into());
    }
    if !(200..300).contains(&status) {
        anyhow::bail!("DuckDuckGo search failed with HTTP {status}");
    }

    let result_selector = Selector::parse(".result").unwrap();
    let link_selector = Selector::parse(".result__a").unwrap();
    let snippet_selector = Selector::parse(".result__snippet").unwrap();
    let base = Url::parse("https://duckduckgo.com/").unwrap();

    let mut results = Vec::new();
    for result in document.select(&result_selector) {
        let Some(link) = result.select(&link_selector).next() else {
            continue;
        };
        // Results link to a redirect (`//duckduckgo.com/l/?uddg=<target>`);
        // a link that is not one is the target itself.
        let href = link.value().attr("href").unwrap_or_default();
        let Ok(href) = base.join(href) else {
            continue;
        };
        let url = href
            .query_pairs()
            .find(|(key, _)| key == "uddg")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_else(|| href.to_string());

        let title = link.text().collect::<String>();
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|s| s.text().collect::<String>())
            .unwrap_or_default();

        results.push(WebSearchResult {
            url,
            title,
            snippet,
            metadata: PageMetadata::default(),
        });
    }
    Ok(results)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchResult {
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub metadata: PageMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebPage {
    pub url: String,
    pub content: String,
    pub metadata: PageMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageMetadata {
    pub date: Option<String>,
    pub domain_score: u8, // 0-100
    pub page_type: PageType,
}

impl Default for PageMetadata {
    fn default() -> Self {
        Self {
            date: None,
            domain_score: 50, // Neutral score for now
            page_type: PageType::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PageType {
    Documentation,
    Blog,
    Forum,
    Academic,
    Other,
}

#[cfg(test)]
mod search_tests {
    use super::*;

    /// The result page's markup, as DuckDuckGo serves it: titles, snippets,
    /// and target URLs taken out of the `uddg` redirect parameter.
    #[test]
    fn parses_the_result_page() {
        let html = include_str!("fixtures/ddg_results.html");
        let results = parse_search_results(200, html).unwrap();
        let found: Vec<(&str, &str, &str)> = results
            .iter()
            .map(|r| (r.url.as_str(), r.title.as_str(), r.snippet.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (
                    "https://example-lang.org/",
                    "Example Programming Language",
                    "A fast and friendly programming language.",
                ),
                (
                    "https://en.example.org/wiki/Example_(programming_language)?x=1&y=2",
                    "Example (programming language) - Wiki",
                    "An encyclopedia article.",
                ),
                (
                    "https://direct.example.net/page",
                    "Direct link without redirect",
                    "",
                ),
            ]
        );
    }

    /// The bot check DuckDuckGo shows to repeated requests is an error of its
    /// own, not an empty result list.
    #[test]
    fn the_bot_check_is_search_blocked() {
        let html = include_str!("fixtures/ddg_blocked.html");
        for status in [202, 200] {
            let error = parse_search_results(status, html).unwrap_err();
            assert!(error.is::<SearchBlocked>(), "status {status}: {error:#}");
        }
    }

    /// Another failed response is an error too, but not a block.
    #[test]
    fn an_error_status_fails_the_search() {
        let error = parse_search_results(503, "").unwrap_err();
        assert!(!error.is::<SearchBlocked>(), "{error:#}");
        assert!(error.to_string().contains("503"), "{error:#}");
    }
}
