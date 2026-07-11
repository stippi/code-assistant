#[cfg(test)]
use super::WebClient;
#[cfg(test)]
use super::{BrowserLaunchConfig, LaunchedBrowser};

/// A persistent profile must carry a logged-in session between launches — the
/// foundation of "act as me". We prove it with a persistent cookie: set it in
/// one browser, then read it back in a fresh browser on the same profile dir.
#[tokio::test]
async fn persistent_profile_preserves_cookies_across_launches() {
    use axum::http::header::{COOKIE, SET_COOKIE};
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::{routing::get, Router};

    async fn set_cookie() -> impl IntoResponse {
        (
            [(SET_COOKIE, "session=abc123; Max-Age=3600; Path=/")],
            "cookie set",
        )
    }
    async fn read_cookie(headers: HeaderMap) -> String {
        headers
            .get(COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    }

    // One server, one port, alive across both launches (same origin ⇒ same
    // cookie jar key).
    let app = Router::new()
        .route("/set", get(set_cookie))
        .route("/read", get(read_cookie));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Persistent profile dir shared by both launches.
    let profile_base = tempfile::tempdir().unwrap();
    let profile_dir = profile_base.path().join("profile");
    let config = BrowserLaunchConfig::persistent(&profile_dir);

    // First launch: receive and store the cookie, then close to flush to disk.
    {
        let mut launched = LaunchedBrowser::launch(config.clone()).await.unwrap();
        let page = launched
            .browser
            .new_page(format!("http://{addr}/set"))
            .await
            .unwrap();
        page.wait_for_navigation().await.unwrap();
        launched.close().await;
    }

    // Second launch on the SAME profile: the cookie should be sent back.
    let mut launched = LaunchedBrowser::launch(config).await.unwrap();
    let page = launched
        .browser
        .new_page(format!("http://{addr}/read"))
        .await
        .unwrap();
    page.wait_for_navigation().await.unwrap();
    let body = page.content().await.unwrap();
    launched.close().await;

    assert!(
        body.contains("session=abc123"),
        "second launch should resend the persisted cookie, got: {body}"
    );
}

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
    assert!(!results[0].url.is_empty());
    assert!(!results[0].title.is_empty());
    assert!(!results[0].snippet.is_empty());
}

#[tokio::test]
async fn test_web_fetch() {
    let client = WebClient::new().await.unwrap();
    let page = client.fetch("https://www.rust-lang.org").await.unwrap();

    println!("\nContent: {}", page.content);

    assert!(!page.content.is_empty());
    assert!(page.content.contains("Rust"));
}
