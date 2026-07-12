#[cfg(test)]
use super::WebClient;
#[cfg(test)]
use super::{BrowserLaunchConfig, LaunchedBrowser};
#[cfg(test)]
use super::{BrowserSession, BrowserSessionManager};

/// Spawn a tiny site: a page with a form, and a submit endpoint that echoes the
/// typed value. Returns the bound address. Used to drive an interactive session
/// deterministically (no real network).
#[cfg(test)]
async fn spawn_form_site() -> std::net::SocketAddr {
    use axum::extract::Query;
    use axum::response::Html;
    use axum::{routing::get, Router};
    use std::collections::HashMap;

    async fn index() -> Html<&'static str> {
        Html(
            "<html><head><title>Login Demo</title></head><body>\
             <h1>Welcome</h1>\
             <form action=\"/submit\" method=\"get\">\
             <input id=\"user\" name=\"user\">\
             <button id=\"go\" type=\"submit\">Go</button>\
             </form></body></html>",
        )
    }
    async fn submit(Query(params): Query<HashMap<String, String>>) -> Html<String> {
        let user = params.get("user").cloned().unwrap_or_default();
        Html(format!(
            "<html><head><title>Submitted</title></head><body>\
             Hello <span id=\"who\">{user}</span></body></html>"
        ))
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/submit", get(submit));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Drive a full interaction: navigate, read, type into a field, click submit,
/// observe the result, take a screenshot — then track it through the manager.
#[tokio::test]
async fn interactive_session_navigates_types_clicks_and_observes() {
    use std::time::Duration;

    let addr = spawn_form_site().await;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();

    // Navigate + read.
    session.navigate(&format!("http://{addr}/")).await.unwrap();
    let obs = session.observe().await.unwrap();
    assert_eq!(obs.title, "Login Demo");
    assert!(obs.text.contains("Welcome"), "got text: {}", obs.text);

    // Type into the field and submit the form.
    session.type_text("#user", "stephan").await.unwrap();
    session.click("#go").await.unwrap();

    // Wait for the result page, then read the echoed value.
    assert!(
        session
            .wait_for("#who", Duration::from_secs(5))
            .await
            .unwrap(),
        "result element should appear after submit"
    );
    let result = session.observe().await.unwrap();
    assert_eq!(result.title, "Submitted");
    assert!(
        result.text.contains("Hello stephan"),
        "form value should round-trip, got: {}",
        result.text
    );

    // Screenshot returns real PNG bytes.
    let png = session.screenshot(false).await.unwrap();
    assert!(png.starts_with(b"\x89PNG"), "screenshot should be a PNG");

    // Manager tracks the session by id and hands back the same instance.
    let manager = BrowserSessionManager::new(4);
    let session = std::sync::Arc::new(session);
    let id = manager.register(session.clone(), "test");
    let fetched = manager.get(id).expect("session should be tracked");
    assert!(std::sync::Arc::ptr_eq(&session, &fetched));
    assert!(manager.remove(id).is_some());
    assert!(manager.get(id).is_none(), "removed session is gone");

    manager.close_all().await;
    session.close().await;
}

/// observe() should surface the page's actionable elements with usable
/// selectors, so the model can target them instead of guessing from a
/// screenshot. The form page has a named input and a submit button.
#[tokio::test]
async fn observe_discovers_interactive_elements_with_selectors() {
    let addr = spawn_form_site().await;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&format!("http://{addr}/")).await.unwrap();

    let obs = session.observe().await.unwrap();
    assert!(
        !obs.elements.is_empty(),
        "should discover elements, got none"
    );

    // The text input has id=user → selector "#user".
    let input = obs
        .elements
        .iter()
        .find(|e| e.selector == "#user")
        .expect("input #user should be discovered");
    assert_eq!(input.role, "text", "input role from its type");

    // The submit button has id=go, text "Go".
    let button = obs
        .elements
        .iter()
        .find(|e| e.selector == "#go")
        .expect("button #go should be discovered");
    assert_eq!(button.label, "Go", "button label from its text");

    session.close().await;
}

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

/// The login "act as me" flow swaps the visible headful window for a headless
/// browser on the same profile after approval. A plain relaunch would lose
/// in-memory **session cookies** (no Max-Age) — so the login would break — which
/// is why we transfer the whole jar via CDP. This proves that transfer restores
/// a session cookie a plain relaunch drops.
#[tokio::test]
async fn session_cookies_survive_a_headless_swap_via_transfer() {
    use axum::http::header::{COOKIE, SET_COOKIE};
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::{routing::get, Router};

    async fn login() -> impl IntoResponse {
        // A session cookie: no Max-Age, so a browser close drops it from disk.
        ([(SET_COOKIE, "sid=secret; Path=/")], "logged in")
    }
    async fn read_cookie(headers: HeaderMap) -> String {
        headers
            .get(COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    }

    let app = Router::new()
        .route("/login", get(login))
        .route("/read", get(read_cookie));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let base = tempfile::tempdir().unwrap();
    let profile_dir = base.path().join("profile");
    let config = BrowserLaunchConfig::persistent(&profile_dir);

    // First (visible) session: log in, capture the jar, close.
    let s1 = BrowserSession::open(config.clone(), "swap").await.unwrap();
    s1.navigate(&format!("http://{addr}/login")).await.unwrap();
    let cookies = s1.export_cookies().await.unwrap();
    assert!(
        cookies.iter().any(|c| c.name == "sid"),
        "the session cookie should be captured before close"
    );
    s1.close().await;

    // Second (headless) session on the same profile. Without the transfer the
    // session cookie is gone after the close — prove that, then restore it.
    let s2 = BrowserSession::open(config, "swap").await.unwrap();
    s2.navigate(&format!("http://{addr}/read")).await.unwrap();
    let before = s2.observe().await.unwrap().text;
    assert!(
        !before.contains("sid=secret"),
        "a plain relaunch should NOT keep the session cookie, got: {before}"
    );

    s2.import_cookies(cookies).await.unwrap();
    s2.navigate(&format!("http://{addr}/read")).await.unwrap();
    let after = s2.observe().await.unwrap().text;
    assert!(
        after.contains("sid=secret"),
        "the transfer should restore the session cookie, got: {after}"
    );
    s2.close().await;
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
