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
    use axum::{Router, routing::get};
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
    use axum::http::HeaderMap;
    use axum::http::header::{COOKIE, SET_COOKIE};
    use axum::response::IntoResponse;
    use axum::{Router, routing::get};

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
    use axum::http::HeaderMap;
    use axum::http::header::{COOKIE, SET_COOKIE};
    use axum::response::IntoResponse;
    use axum::{Router, routing::get};

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

/// A self-contained page (base64 data URL, no server) with an input whose
/// value can be read back, plus recorders for the last key event's modifiers.
#[cfg(test)]
fn data_url(html: &str) -> String {
    use base64::Engine;
    format!(
        "data:text/html;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(html)
    )
}

/// A chord like `Control+a` must reach the page with the modifier flag set, not
/// error out with "Key not found: Control+a". The page records the last keydown
/// as "<key>|ctrl:<bool>|meta:<bool>|shift:<bool>".
#[tokio::test]
async fn press_key_supports_modifier_chords() {
    let html = concat!(
        "<html><body>",
        "<input id=\"f\" onkeydown=\"document.title=event.key+'|ctrl:'+event.ctrlKey+'|meta:'+event.metaKey+'|shift:'+event.shiftKey\">",
        "</body></html>"
    );
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&data_url(html)).await.unwrap();

    // Control+a: must not error, and ctrlKey must be true on the event.
    session.press_key("#f", "Control+a").await.unwrap();
    let title = session.observe().await.unwrap().title;
    assert!(
        title.contains("ctrl:true"),
        "Control+a should set the ctrl modifier, got title: {title}"
    );

    // Shift+Tab is a common chord too.
    session.press_key("#f", "Shift+Tab").await.unwrap();
    let title = session.observe().await.unwrap().title;
    assert!(
        title.contains("shift:true"),
        "Shift+Tab should set the shift modifier, got title: {title}"
    );

    session.close().await;
}

/// `fill` should REPLACE the field's content, not append to it (the old
/// `type_text` appended, forcing manual End+Backspace clearing).
#[tokio::test]
async fn fill_replaces_existing_field_content() {
    let html = "<html><body><input id=\"f\" value=\"prefilled\"></body></html>";
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&data_url(html)).await.unwrap();

    session.fill("#f", "replacement").await.unwrap();
    let value = session
        .eval("document.getElementById('f').value")
        .await
        .unwrap();
    assert_eq!(
        value.as_str().unwrap_or_default(),
        "replacement",
        "fill should replace, not append"
    );

    session.close().await;
}

/// `clear` should empty a prefilled field.
#[tokio::test]
async fn clear_empties_a_field() {
    let html = "<html><body><input id=\"f\" value=\"prefilled\"></body></html>";
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&data_url(html)).await.unwrap();

    session.clear("#f").await.unwrap();
    let value = session
        .eval("document.getElementById('f').value")
        .await
        .unwrap();
    assert_eq!(
        value.as_str().unwrap_or_default(),
        "",
        "clear should empty the field"
    );

    session.close().await;
}

/// A `text=` / `role=` selector should resolve to the element by its visible
/// text or ARIA role, not by a fragile hashed CSS id.
#[tokio::test]
async fn click_resolves_text_and_role_selectors() {
    let html = concat!(
        "<html><body>",
        "<button id=\"h4a3f\" onclick=\"document.title='clicked-'+this.id\">Speichern und Verlassen</button>",
        "</body></html>"
    );
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&data_url(html)).await.unwrap();

    // By visible text (substring, case-insensitive).
    session.click("text=Speichern und Verlassen").await.unwrap();
    let title = session.observe().await.unwrap().title;
    assert!(
        title.contains("clicked-h4a3f"),
        "text= selector should click the button, got title: {title}"
    );

    session.close().await;
}

/// Discovery must reach into a modal/dialog that has its own scroll container
/// and enumerate its buttons — those were previously missing (finding 4).
#[tokio::test]
async fn observe_discovers_elements_inside_a_dialog() {
    let html = concat!(
        "<html><body>",
        "<div id=\"bg\">background</div>",
        "<div role=\"dialog\" style=\"position:fixed;inset:20px;overflow:auto\">",
        "<button id=\"dl\">Herunterladen</button>",
        "<button id=\"cl\">Schließen</button>",
        "</div>",
        "</body></html>"
    );
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&data_url(html)).await.unwrap();

    let obs = session.observe().await.unwrap();
    assert!(
        obs.elements.iter().any(|e| e.selector == "#dl"),
        "dialog button should be discovered, got: {:?}",
        obs.elements
    );

    session.close().await;
}

/// `observe_text_light` should skip the (expensive, redundant) full innerText
/// dump while still returning the interactive elements (finding 7).
#[tokio::test]
async fn observe_can_skip_the_text_dump() {
    let html = "<html><body><h1>Big page</h1><button id=\"go\">Go</button></body></html>";
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&data_url(html)).await.unwrap();

    let obs = session.observe_with(false).await.unwrap();
    assert!(obs.text.is_empty(), "text should be suppressed");
    assert!(
        obs.elements.iter().any(|e| e.selector == "#go"),
        "elements should still be present"
    );

    session.close().await;
}

/// Scrolling with a selector AND a delta must move the dialog's own scroll
/// container, not the page behind it (finding 3).
#[tokio::test]
async fn scroll_moves_a_dialog_inner_container() {
    let html = concat!(
        "<html><body style=\"height:4000px\">",
        "<div id=\"panel\" style=\"position:fixed;top:0;left:0;width:200px;height:150px;overflow:auto\">",
        "<div style=\"height:2000px\">tall inner content</div>",
        "</div>",
        "</body></html>"
    );
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&data_url(html)).await.unwrap();

    session.scroll(Some("#panel"), 0.0, 500.0).await.unwrap();

    let panel_top = session
        .eval("document.getElementById('panel').scrollTop")
        .await
        .unwrap();
    assert!(
        panel_top.as_f64().unwrap_or(0.0) > 100.0,
        "the dialog's inner container should have scrolled, scrollTop={panel_top}"
    );
    // The page itself should NOT have moved.
    let page_y = session.eval("window.scrollY").await.unwrap();
    assert!(
        page_y.as_f64().unwrap_or(1.0) < 1.0,
        "the page behind the dialog should not scroll, scrollY={page_y}"
    );

    session.close().await;
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

/// A page with an `alert` button and a `confirm` button that writes the user's
/// answer into `#out`.
#[cfg(test)]
async fn spawn_dialog_site() -> std::net::SocketAddr {
    use axum::response::Html;
    use axum::{Router, routing::get};

    async fn index() -> Html<&'static str> {
        Html(
            "<html><head><title>Dialogs</title></head><body>\
             <button id=\"alert\" onclick=\"alert('Hallo')\">Alert</button>\
             <button id=\"confirm\" onclick=\"document.getElementById('out').textContent = \
             confirm('Sicher?') ? 'yes' : 'no'\">Confirm</button>\
             <span id=\"out\"></span></body></html>",
        )
    }

    let app = Router::new().route("/", get(index));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// A JavaScript dialog blocks the renderer until someone answers it; left open,
/// the click that raised it and every later CDP command hang for minutes. The
/// session must answer dialogs itself and report them in the next observation.
#[tokio::test]
async fn javascript_dialogs_are_answered_and_reported() {
    use std::time::Duration;

    let addr = spawn_dialog_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&format!("http://{addr}/")).await.unwrap();

    // An alert is acknowledged; the click returns and the page stays usable.
    tokio::time::timeout(Duration::from_secs(10), session.click("#alert"))
        .await
        .expect("click raising an alert must not hang")
        .unwrap();
    let obs = session.observe().await.unwrap();
    assert_eq!(obs.dialogs.len(), 1, "got: {:?}", obs.dialogs);
    assert_eq!(obs.dialogs[0].kind, "alert");
    assert_eq!(obs.dialogs[0].message, "Hallo");
    assert!(obs.dialogs[0].accepted);

    // Reported once: the next observation starts clean.
    assert!(session.observe().await.unwrap().dialogs.is_empty());

    // A confirm is dismissed by default — accepting could trigger an outward
    // action the model never saw the warning for.
    tokio::time::timeout(Duration::from_secs(10), session.click("#confirm"))
        .await
        .expect("click raising a confirm must not hang")
        .unwrap();
    let obs = session.observe().await.unwrap();
    assert_eq!(obs.dialogs.len(), 1, "got: {:?}", obs.dialogs);
    assert_eq!(obs.dialogs[0].kind, "confirm");
    assert_eq!(obs.dialogs[0].message, "Sicher?");
    assert!(!obs.dialogs[0].accepted);
    assert_eq!(
        session
            .eval("document.getElementById('out').textContent")
            .await
            .unwrap(),
        "no"
    );

    // Opting in accepts it.
    session.set_accept_dialogs(true);
    tokio::time::timeout(Duration::from_secs(10), session.click("#confirm"))
        .await
        .expect("click raising a confirm must not hang")
        .unwrap();
    let obs = session.observe().await.unwrap();
    assert!(obs.dialogs[0].accepted);
    assert_eq!(
        session
            .eval("document.getElementById('out').textContent")
            .await
            .unwrap(),
        "yes"
    );

    session.close().await;
}

/// German (and any non-US-layout) text must type: `ü`, `ß` and `€` are not in
/// chromiumoxide's US key table, which used to fail with "Key not found".
#[tokio::test]
async fn type_and_fill_handle_characters_outside_the_us_layout() {
    let addr = spawn_form_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&format!("http://{addr}/")).await.unwrap();

    session.type_text("#user", "Grüße, ").await.unwrap();
    session.type_text("#user", "Straße 5 € ✓").await.unwrap();
    let value = session
        .eval("document.getElementById('user').value")
        .await
        .unwrap();
    assert_eq!(value, "Grüße, Straße 5 € ✓");

    session.fill("#user", "Übermut").await.unwrap();
    let value = session
        .eval("document.getElementById('user').value")
        .await
        .unwrap();
    assert_eq!(value, "Übermut");

    session.close().await;
}

/// A page that can be told to hang: `#spin` starts an endless script loop
/// (after the click has returned), and its iframe never finishes loading, so
/// the page's `load` event never fires.
#[cfg(test)]
async fn spawn_hanging_site() -> std::net::SocketAddr {
    use axum::response::Html;
    use axum::{Router, routing::get};

    async fn index() -> Html<&'static str> {
        Html(
            "<html><head><title>Busy</title></head><body>\
             <button id=\"spin\" onclick=\"setTimeout(() => { while (true) {} }, 50)\">Spin</button>\
             </body></html>",
        )
    }
    async fn slow_frame() -> Html<&'static str> {
        Html(
            "<html><head><title>Slow</title></head><body>\
             <h1>Main content</h1><iframe src=\"/never\"></iframe></body></html>",
        )
    }
    async fn never() -> Html<&'static str> {
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        Html("")
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/slow", get(slow_frame))
        .route("/never", get(never));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

#[cfg(test)]
fn short_timeouts() -> super::BrowserTimeouts {
    use std::time::Duration;
    super::BrowserTimeouts {
        command: Duration::from_secs(1),
        navigation: Duration::from_secs(2),
    }
}

/// A page that never fires `load` fails the navigation within the limit with a
/// `BrowserTimeout`, and what did load is still observable.
#[tokio::test]
async fn navigation_that_never_loads_times_out_but_the_page_is_usable() {
    use std::time::{Duration, Instant};

    let addr = spawn_hanging_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap()
        .with_timeouts(short_timeouts());

    let start = Instant::now();
    let err = session
        .navigate(&format!("http://{addr}/slow"))
        .await
        .expect_err("load never fires");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert!(
        err.downcast_ref::<super::BrowserTimeout>().is_some(),
        "{err}"
    );

    let obs = session.observe().await.unwrap();
    assert!(obs.text.contains("Main content"), "got: {}", obs.text);
    session.close().await;
}

/// A page stuck in a script loop answers no CDP command. Every verb must give
/// up within its limit instead of hanging the agent for minutes.
#[tokio::test]
async fn a_hung_page_fails_fast_instead_of_hanging() {
    use std::time::{Duration, Instant};

    let addr = spawn_hanging_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap()
        .with_timeouts(short_timeouts());
    session.navigate(&format!("http://{addr}/")).await.unwrap();
    session.click("#spin").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let limit = Duration::from_secs(3);
    let is_timeout = |e: &anyhow::Error| e.downcast_ref::<super::BrowserTimeout>().is_some();

    let start = Instant::now();
    let err = session.observe().await.expect_err("page is hung");
    assert!(
        start.elapsed() < limit,
        "observe took {:?}",
        start.elapsed()
    );
    assert!(is_timeout(&err), "{err}");

    let start = Instant::now();
    let err = session.screenshot(false).await.expect_err("page is hung");
    assert!(
        start.elapsed() < limit,
        "screenshot took {:?}",
        start.elapsed()
    );
    assert!(is_timeout(&err), "{err}");

    let start = Instant::now();
    let err = session.click("#spin").await.expect_err("page is hung");
    assert!(start.elapsed() < limit, "click took {:?}", start.elapsed());
    assert!(is_timeout(&err), "{err}");

    let start = Instant::now();
    let appeared = session
        .wait_for("#nothing", Duration::from_secs(1))
        .await
        .unwrap();
    assert!(!appeared);
    assert!(
        start.elapsed() < limit,
        "wait_for took {:?}",
        start.elapsed()
    );

    let start = Instant::now();
    session.settle().await;
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "settle took {:?}",
        start.elapsed()
    );

    let start = Instant::now();
    session.close().await;
    assert!(
        start.elapsed() < Duration::from_secs(12),
        "close took {:?}",
        start.elapsed()
    );
}

/// Tabs: a fresh browser has one active tab; tabs can be created, selected
/// and closed; a page-opened popup (`target=_blank`) is adopted as a new tab.
#[tokio::test]
async fn tabs_are_created_selected_closed_and_popups_adopted() {
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tabs = session.tabs().await;
    assert_eq!(tabs.len(), 1);
    assert!(tabs[0].active);
    let first = tabs[0].id.clone();

    // A background tab does not take over; a selected one does.
    let second = session.create_tab(false).await.unwrap();
    assert_eq!(session.active_tab().unwrap().id(), first);
    session.select_tab(second.id()).unwrap();
    assert_eq!(session.active_tab().unwrap().id(), second.id());

    // Closing the active tab activates another.
    session.close_tab(second.id()).await.unwrap();
    assert_eq!(session.active_tab().unwrap().id(), first);
    assert!(session.tab(Some(second.id())).is_err());

    // A link with target=_blank opens a tab the session adopts.
    let page = data_url(
        "<html><body><a id=\"pop\" target=\"_blank\" \
         href=\"data:text/html,<title>Popup</title>hi\">open</a></body></html>",
    );
    session.navigate(&page).await.unwrap();
    session.click("#pop").await.unwrap();
    let mut adopted = Vec::new();
    for _ in 0..30 {
        adopted.extend(session.sync_tabs().await.unwrap());
        if !adopted.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(adopted.len(), 1, "the popup should be adopted");
    assert_eq!(
        session.active_tab().unwrap().id(),
        first,
        "a popup does not steal the active tab"
    );
    assert_eq!(session.tabs().await.len(), 2);

    session.close().await;
}

/// Headless browsers get a desktop-sized viewport, not chromiumoxide's 800×600.
#[tokio::test]
async fn headless_viewport_is_desktop_sized() {
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    assert_eq!(session.viewport_size().await.unwrap(), (1280.0, 800.0));
    session.close().await;
}

/// The accessibility snapshot names elements by role and label and hands out
/// refs that resolve to clickable points.
#[tokio::test]
async fn read_page_lists_elements_with_refs_that_resolve_to_points() {
    let addr = spawn_form_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&format!("http://{addr}/")).await.unwrap();
    let tab = session.active_tab().unwrap();

    let tree = tab.read_page(false, None, 15).await.unwrap().join("\n");
    assert!(
        tree.contains(r#"- document "Login Demo" [ref_"#),
        "got:\n{tree}"
    );
    assert!(tree.contains(r#"- heading "Welcome""#), "got:\n{tree}");

    let interactive = tab.read_page(true, None, 15).await.unwrap();
    assert_eq!(
        interactive.len(),
        2,
        "textbox and button, got: {interactive:?}"
    );
    assert!(interactive[0].starts_with("- textbox"), "{interactive:?}");
    assert!(
        interactive[1].starts_with(r#"- button "Go""#),
        "{interactive:?}"
    );

    let found = tab.find("go", 20).await.unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let go_ref = found[0]
        .split(['[', ']'])
        .nth(1)
        .expect("a ref in the found line")
        .to_string();
    let point = tab.ref_point(&go_ref).await.unwrap();
    assert!(point.x > 0.0 && point.y > 0.0, "{point:?}");
    assert!(tab.ref_point("ref_999").await.is_err());

    session.close().await;
}

/// A page that records mouse events on `#box` (and mouseup anywhere), with an
/// input, an inner scroll container, and a tall body so the window scrolls.
#[cfg(test)]
fn input_lab_url() -> String {
    data_url(
        "<html><body style=\"margin:0;height:3000px\">\
         <input id=\"f\" style=\"position:absolute;left:100px;top:100px;width:200px\">\
         <div id=\"box\" style=\"position:absolute;left:400px;top:100px;width:100px;height:100px\"></div>\
         <div id=\"scroller\" style=\"position:absolute;left:600px;top:100px;width:200px;height:100px;overflow:auto\">\
         <div style=\"height:1000px\">x</div></div>\
         <script>\
         window.log = [];\
         const rec = (e) => window.log.push(e.type + ':' + e.button + ':' + e.detail + ':' \
           + Math.round(e.clientX) + ',' + Math.round(e.clientY) + (e.ctrlKey ? ':ctrl' : ''));\
         const box = document.getElementById('box');\
         ['mousedown', 'dblclick', 'contextmenu', 'mouseover'].forEach((t) => box.addEventListener(t, rec));\
         document.addEventListener('mouseup', rec);\
         </script></body></html>",
    )
}

#[cfg(test)]
fn png_size(png: &[u8]) -> (u32, u32) {
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    (be(&png[16..20]), be(&png[20..24]))
}

#[tokio::test]
async fn mouse_and_keyboard_reach_the_page() {
    use super::Button;
    use chromiumoxide::layout::Point;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&input_lab_url()).await.unwrap();
    let tab = session.active_tab().unwrap();
    let log = || async { tab.eval("window.log.splice(0).join(' ')").await.unwrap() };

    // Click into the field, type, then fix a typo with repeated Backspace.
    tab.click_point(Point { x: 150.0, y: 110.0 }, Button::Left, 1, 0)
        .await
        .unwrap();
    tab.type_into_focused("Grüße!!").await.unwrap();
    tab.press_keys("Backspace", 2).await.unwrap();
    tab.press_keys("left right End", 1).await.unwrap();
    assert_eq!(
        tab.eval("document.getElementById('f').value")
            .await
            .unwrap(),
        "Grüße"
    );
    assert!(tab.press_keys("NoSuchKey", 1).await.is_err());
    log().await;

    // Hover, a ctrl-click, a double click and a right click on the box.
    let center = Point { x: 450.0, y: 150.0 };
    tab.hover_point(center).await.unwrap();
    tab.click_point(center, Button::Left, 1, 2).await.unwrap();
    tab.click_point(center, Button::Left, 2, 0).await.unwrap();
    tab.click_point(center, Button::Right, 1, 0).await.unwrap();
    let events = log().await;
    let events = events.as_str().unwrap();
    assert!(events.starts_with("mouseover"), "{events}");
    assert!(events.contains("mousedown:0:1:450,150:ctrl"), "{events}");
    assert!(events.contains("dblclick:0:2:450,150"), "{events}");
    assert!(events.contains("contextmenu:2:"), "{events}");

    // A drag ends where it was released.
    tab.drag(center, Point { x: 700.0, y: 500.0 })
        .await
        .unwrap();
    let events = log().await;
    assert!(
        events.as_str().unwrap().ends_with("mouseup:0:1:700,500"),
        "{events}"
    );

    // The wheel scrolls the container under the pointer, not the page.
    tab.wheel(Point { x: 700.0, y: 150.0 }, 0.0, 200.0)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        tab.eval("[document.getElementById('scroller').scrollTop, window.scrollY]")
            .await
            .unwrap(),
        serde_json::json!([200, 0])
    );

    session.close().await;
}

#[tokio::test]
async fn screenshots_define_the_coordinate_frame() {
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    session.navigate(&input_lab_url()).await.unwrap();
    let tab = session.active_tab().unwrap();

    let shot = tab.screenshot_frame(None).await.unwrap();
    assert_eq!((shot.width, shot.height), (1280, 800));
    assert_eq!(png_size(&shot.png), (1280, 800));
    assert_eq!(tab.frame_point(100.0, 50.0).x, 100.0);

    // A half-size screenshot halves the frame: its coordinates map back up.
    let half = tab.screenshot_frame(Some(0.5)).await.unwrap();
    assert_eq!(png_size(&half.png), (640, 400));
    let p = tab.frame_point(320.0, 200.0);
    assert_eq!((p.x, p.y), (640.0, 400.0));

    // Zooming into the frame's top-left quarter enlarges it to the edge
    // limit, and leaves the frame alone.
    let zoomed = tab.zoom([0.0, 0.0, 320.0, 200.0], None).await.unwrap();
    assert_eq!(png_size(&zoomed.png), (zoomed.width, zoomed.height));
    assert_eq!(zoomed.width, super::MAX_SCREENSHOT_EDGE);
    assert_eq!(tab.frame_point(320.0, 200.0).x, 640.0);

    // On a scrolled page the screenshot shows the viewport, not the top of
    // the document.
    tab.eval("window.scrollTo(0, 1000)").await.unwrap();
    let ours = tab.screenshot_frame(None).await.unwrap();
    let reference = tab.screenshot(false).await.unwrap();
    assert!(
        ours.png == reference,
        "scrolled screenshot differs from the viewport"
    );

    session.close().await;
}
