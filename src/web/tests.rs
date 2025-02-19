#[cfg(test)]
mod tests {
    use super::super::WebClient;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_web_search() {
        let client = WebClient::new().await.unwrap();
        let results = client.search("rust programming", 1).await.unwrap();

        println!("\nSearch Results:");
        for (i, result) in results.iter().enumerate() {
            println!("\n{}. {}", i + 1, result.title);
            println!("   URL: {}", result.url);
            println!("   Snippet: {}", result.snippet);
        }

        assert!(!results.is_empty());
        assert!(results[0].url.len() > 0);
        assert!(results[0].title.len() > 0);
        assert!(results[0].snippet.len() > 0);
    }

    #[tokio::test]
    async fn test_web_fetch() {
        let client = WebClient::new().await.unwrap();
        let page = client.fetch("https://www.rust-lang.org").await.unwrap();

        sleep(Duration::from_secs(2)).await;

        assert!(page.content.len() > 0);
        assert!(page.content.contains("Rust"));
    }
}
