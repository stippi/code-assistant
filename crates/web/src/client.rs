use anyhow::Result;
use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;
use htmd::{Element, HtmlToMarkdown};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use url::Url;

pub struct WebClient {
    http_client: Client,
    browser: Browser,
    _user_data_dir: TempDir,
}

impl WebClient {
    pub async fn new() -> Result<Self> {
        // Create temporary user data directory
        let user_data_dir = tempfile::tempdir()?;

        let (browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
                //.with_head()
                .user_data_dir(user_data_dir.path())
                .build()
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        )
        .await?;

        // Run browser handler in background
        tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(e) = event {
                    eprintln!("Browser handler error: {e}");
                }
            }
        });

        Ok(Self {
            http_client: Client::new(),
            browser,
            _user_data_dir: user_data_dir,
        })
    }

    pub async fn search(&self, query: &str, page: u32) -> Result<Vec<WebSearchResult>> {
        let search_url = format!(
            "https://html.duckduckgo.com/html/?q={}&s={}",
            utf8_percent_encode(query, NON_ALPHANUMERIC),
            (page - 1) * 20
        );

        let resp = self.http_client.get(&search_url).send().await?;
        let html = resp.text().await?;
        let document = Html::parse_document(&html);

        let result_selector = Selector::parse(".result").unwrap();
        let link_selector = Selector::parse(".result__a").unwrap();
        let snippet_selector = Selector::parse(".result__snippet").unwrap();

        let mut results = Vec::new();
        for result in document.select(&result_selector) {
            if let Some(link) = result.select(&link_selector).next() {
                let encoded_url = link.value().attr("href").unwrap_or_default();

                // Parse the redirect URL
                let redirect_url = Url::parse(&format!("https:{encoded_url}"))?;

                // Get the actual URL from the 'uddg' parameter
                let url = redirect_url
                    .query_pairs()
                    .find(|(key, _)| key == "uddg")
                    .map(|(_, value)| value.to_string())
                    .unwrap_or_default();

                let title = link.text().collect::<String>();
                let snippet = result
                    .select(&snippet_selector)
                    .next()
                    .map(|s| s.text().collect::<String>())
                    .unwrap_or_default();

                results.push(WebSearchResult {
                    url: url.to_string(),
                    title,
                    snippet,
                    metadata: PageMetadata::default(),
                });
            }
        }

        Ok(results)
    }

    pub async fn fetch(&self, url: &str) -> Result<WebPage> {
        let url = Url::parse(url)?;
        let page = self.browser.new_page(url.as_str()).await?;

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
