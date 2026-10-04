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

/// Drive a full interaction: navigate, read the tree, click a field by ref,
/// type, submit with Enter, read the result — then track it through the
/// manager.
#[tokio::test]
async fn interactive_session_navigates_types_and_submits() {
    use super::Button;

    let addr = spawn_form_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&format!("http://{addr}/")).await.unwrap();

    let found = tab.find("textbox", 5).await.unwrap();
    let field = found[0].split(['[', ']']).nth(1).unwrap().to_string();
    let at = tab.ref_point(&field).await.unwrap();
    tab.click_point(at, Button::Left, 1, 0).await.unwrap();
    tab.type_into_focused("stephan").await.unwrap();
    tab.press_keys("Enter", 1).await.unwrap();
    tab.settle().await;

    assert_eq!(tab.javascript("document.title").await.unwrap(), "Submitted");
    assert!(tab.page_text(1000).await.unwrap().contains("Hello stephan"));

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
    s1.active_tab()
        .unwrap()
        .navigate(&format!("http://{addr}/login"))
        .await
        .unwrap();
    let cookies = s1.export_cookies().await.unwrap();
    assert!(
        cookies.iter().any(|c| c.name == "sid"),
        "the session cookie should be captured before close"
    );
    s1.close().await;

    // Second (headless) session on the same profile. Without the transfer the
    // session cookie is gone after the close — prove that, then restore it.
    let s2 = BrowserSession::open(config, "swap").await.unwrap();
    let tab = s2.active_tab().unwrap();
    tab.navigate(&format!("http://{addr}/read")).await.unwrap();
    let before = tab.page_text(1000).await.unwrap();
    assert!(
        !before.contains("sid=secret"),
        "a plain relaunch should NOT keep the session cookie, got: {before}"
    );

    tab.import_cookies(cookies).await.unwrap();
    tab.navigate(&format!("http://{addr}/read")).await.unwrap();
    let after = tab.page_text(1000).await.unwrap();
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

/// A chord like `ctrl+a` must reach the page with the modifier flag set. The
/// page records the last keydown as "<key>|ctrl:<bool>|meta:<bool>|shift:<bool>".
#[tokio::test]
async fn key_chords_set_their_modifiers() {
    let html = concat!(
        "<html><body>",
        "<input id=\"f\" autofocus onkeydown=\"document.title=event.key+'|ctrl:'+event.ctrlKey+'|meta:'+event.metaKey+'|shift:'+event.shiftKey\">",
        "</body></html>"
    );
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&data_url(html)).await.unwrap();
    tab.javascript("document.getElementById('f').focus()")
        .await
        .unwrap();

    tab.press_keys("Control+a", 1).await.unwrap();
    let title = tab.javascript("document.title").await.unwrap();
    assert!(title.contains("ctrl:true"), "got title: {title}");

    tab.press_keys("shift+Tab", 1).await.unwrap();
    let title = tab.javascript("document.title").await.unwrap();
    assert!(
        title.starts_with("Tab|") && title.contains("shift:true"),
        "got title: {title}"
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
    use super::Button;
    use std::time::Duration;

    let addr = spawn_dialog_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&format!("http://{addr}/")).await.unwrap();
    let click = |name: &'static str| {
        let tab = tab.clone();
        async move {
            let line = tab.find(name, 1).await.unwrap().remove(0);
            let r = line.split(['[', ']']).nth(1).unwrap().to_string();
            let at = tab.ref_point(&r).await.unwrap();
            tokio::time::timeout(
                Duration::from_secs(10),
                tab.click_point(at, Button::Left, 1, 0),
            )
            .await
            .expect("a click raising a dialog must not hang")
            .unwrap();
        }
    };
    let out = || async {
        tab.javascript("document.getElementById('out').textContent")
            .await
            .unwrap()
    };

    // An alert is acknowledged; the click returns and the page stays usable.
    click("button \"Alert\"").await;
    let dialogs = tab.take_dialogs();
    assert_eq!(dialogs.len(), 1, "got: {dialogs:?}");
    assert_eq!(dialogs[0].kind, "alert");
    assert_eq!(dialogs[0].message, "Hallo");
    assert!(dialogs[0].accepted);

    // Reported once.
    assert!(tab.take_dialogs().is_empty());

    // A confirm is dismissed by default — accepting could trigger an outward
    // action the model never saw the warning for.
    click("button \"Confirm\"").await;
    let dialogs = tab.take_dialogs();
    assert_eq!(dialogs.len(), 1, "got: {dialogs:?}");
    assert_eq!(dialogs[0].kind, "confirm");
    assert_eq!(dialogs[0].message, "Sicher?");
    assert!(!dialogs[0].accepted);
    assert_eq!(out().await, "no");

    // Opting in accepts it.
    tab.set_accept_dialogs(true);
    click("button \"Confirm\"").await;
    assert!(tab.take_dialogs()[0].accepted);
    assert_eq!(out().await, "yes");

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

    let tab = session.active_tab().unwrap();
    let start = Instant::now();
    let err = tab
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

    let text = tab.page_text(1000).await.unwrap();
    assert!(text.contains("Main content"), "got: {text}");
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
    let tab = session.active_tab().unwrap();
    tab.navigate(&format!("http://{addr}/")).await.unwrap();
    let line = tab.find("Spin", 1).await.unwrap().remove(0);
    let spin = tab
        .ref_point(line.split(['[', ']']).nth(1).unwrap())
        .await
        .unwrap();
    // Start the endless loop with a margin, so the setup itself never races
    // the hang (a click on #spin could, on a loaded machine).
    tab.javascript("setTimeout(() => { while (true) {} }, 100); 0")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let limit = Duration::from_secs(3);
    let is_timeout = |e: &anyhow::Error| e.downcast_ref::<super::BrowserTimeout>().is_some();

    let start = Instant::now();
    let err = tab
        .read_page(false, None, 15)
        .await
        .expect_err("page is hung");
    assert!(
        start.elapsed() < limit,
        "read_page took {:?}",
        start.elapsed()
    );
    assert!(is_timeout(&err), "{err}");

    let start = Instant::now();
    let err = tab
        .screenshot_frame(None)
        .await
        .err()
        .expect("page is hung");
    assert!(
        start.elapsed() < limit,
        "screenshot took {:?}",
        start.elapsed()
    );
    assert!(is_timeout(&err), "{err}");

    let start = Instant::now();
    let err = tab
        .click_point(spin, super::Button::Left, 1, 0)
        .await
        .expect_err("page is hung");
    assert!(start.elapsed() < limit, "click took {:?}", start.elapsed());
    assert!(is_timeout(&err), "{err}");

    let start = Instant::now();
    let err = tab.javascript("1").await.expect_err("page is hung");
    assert!(
        start.elapsed() < limit,
        "javascript took {:?}",
        start.elapsed()
    );
    assert!(is_timeout(&err), "{err}");

    let start = Instant::now();
    tab.settle().await;
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
    let tab = session.active_tab().unwrap();
    tab.navigate(&page).await.unwrap();
    let line = tab.find("link", 1).await.unwrap().remove(0);
    let at = tab
        .ref_point(line.split(['[', ']']).nth(1).unwrap())
        .await
        .unwrap();
    tab.click_point(at, super::Button::Left, 1, 0)
        .await
        .unwrap();
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
    assert_eq!(
        session.active_tab().unwrap().viewport_size().await.unwrap(),
        (1280.0, 800.0)
    );
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
    let tab = session.active_tab().unwrap();
    tab.navigate(&format!("http://{addr}/")).await.unwrap();

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
         document.addEventListener('dragstart', rec);\
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
    let tab = session.active_tab().unwrap();
    tab.navigate(&input_lab_url()).await.unwrap();
    let log = || async {
        tab.javascript("window.log.splice(0).join(' ')")
            .await
            .unwrap()
    };

    // Click into the field, type, then fix a typo with repeated Backspace.
    tab.click_point(Point { x: 150.0, y: 110.0 }, Button::Left, 1, 0)
        .await
        .unwrap();
    tab.type_into_focused("Grüße!!").await.unwrap();
    tab.press_keys("Backspace", 2).await.unwrap();
    tab.press_keys("left right End", 1).await.unwrap();
    assert_eq!(
        tab.javascript("document.getElementById('f').value")
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
    assert!(events.starts_with("mouseover"), "{events}");
    assert!(events.contains("mousedown:0:1:450,150:ctrl"), "{events}");
    assert!(events.contains("dblclick:0:2:450,150"), "{events}");
    assert!(events.contains("contextmenu:2:"), "{events}");

    // A drag ends where it was released. Pressing on selected text starts a
    // native drag-and-drop instead (no mouseup), and what the double click
    // above leaves selected differs between platforms — so clear it first.
    tab.javascript("getSelection().removeAllRanges()")
        .await
        .unwrap();
    tab.drag(center, Point { x: 700.0, y: 500.0 })
        .await
        .unwrap();
    let events = log().await;
    assert!(events.ends_with("mouseup:0:1:700,500"), "{events}");

    // The wheel scrolls the container under the pointer, not the page.
    tab.wheel(Point { x: 700.0, y: 150.0 }, 0.0, 200.0)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        tab.javascript("[document.getElementById('scroller').scrollTop, window.scrollY]")
            .await
            .unwrap(),
        "[200,0]"
    );

    session.close().await;
}

#[tokio::test]
async fn screenshots_define_the_coordinate_frame() {
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&input_lab_url()).await.unwrap();

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
    tab.javascript("window.scrollTo(0, 1000)").await.unwrap();
    let ours = tab.screenshot_frame(None).await.unwrap();
    let reference = tab
        .page()
        .screenshot(
            chromiumoxide::page::ScreenshotParams::builder()
                .format(chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Png)
                .build(),
        )
        .await
        .unwrap();
    assert!(
        ours.png == reference,
        "scrolled screenshot differs from the viewport"
    );

    session.close().await;
}

/// A page that logs to the console, throws, and fetches a JSON API and a
/// missing resource; plus a form with a select, a checkbox and a text field.
#[cfg(test)]
async fn spawn_devtools_site() -> std::net::SocketAddr {
    use axum::response::{Html, IntoResponse};
    use axum::{Router, routing::get};

    async fn index() -> Html<&'static str> {
        Html(
            "<html><head><title>Devtools</title></head><body>\
             <main><h1>Main part</h1><p>Only this.</p></main><footer>Footer</footer>\
             <label>Color <select id=\"color\"><option value=\"r\">Red</option>\
             <option value=\"g\">Green</option></select></label>\
             <label><input type=\"checkbox\" id=\"agree\"> Agree</label>\
             <label>Name <input id=\"name\"></label>\
             <script>\
             window.changes = [];\
             for (const id of ['color', 'agree', 'name']) {\
               document.getElementById(id).addEventListener('change', (e) => window.changes.push(id));\
             }\
             console.log('hello', 42, {a: 1});\
             console.error('something broke');\
             setTimeout(() => { throw new Error('uncaught boom'); }, 0);\
             fetch('/api/items').then((r) => r.json()).then((j) => console.log('items', j.items.length));\
             fetch('/missing');\
             </script></body></html>",
        )
    }
    async fn items() -> impl IntoResponse {
        axum::Json(serde_json::json!({"items": [1, 2, 3]}))
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/api/items", get(items));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

#[tokio::test]
async fn console_network_and_javascript_are_inspectable() {
    let addr = spawn_devtools_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&format!("http://{addr}/")).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let all = tab.console_messages(false, None, 50).join("\n");
    assert!(all.contains("[log] hello 42"), "{all}");
    assert!(all.contains("[log] items 3"), "{all}");
    let errors = tab.console_messages(true, None, 50).join("\n");
    assert!(errors.contains("[error] something broke"), "{errors}");
    assert!(
        errors.contains("[exception]") && errors.contains("uncaught boom"),
        "{errors}"
    );
    assert!(
        errors.contains("404"),
        "the failed load is logged: {errors}"
    );
    assert!(!errors.contains("hello"), "{errors}");
    assert_eq!(tab.console_messages(false, Some("BOOM"), 50).len(), 1);
    assert_eq!(tab.console_messages(false, None, 2).len(), 2);

    let requests = tab.network_requests(None, 50).join("\n");
    assert!(
        requests.contains("GET 200 fetch") && requests.contains("/api/items"),
        "{requests}"
    );
    assert!(
        requests.contains("GET 404") && requests.contains("/missing"),
        "{requests}"
    );
    let api = tab.network_requests(Some("/api/"), 50);
    assert_eq!(api.len(), 1, "{api:?}");
    let id = api[0].trim_start_matches('[').split(']').next().unwrap();
    let body = tab.response_body(id, 1000).await.unwrap();
    assert_eq!(body, r#"{"items":[1,2,3]}"#);

    // REPL semantics: top-level await, the last expression's value.
    assert_eq!(
        tab.javascript("const n = await Promise.resolve(21); n * 2")
            .await
            .unwrap(),
        "42"
    );
    assert_eq!(tab.javascript("document.title").await.unwrap(), "Devtools");
    assert_eq!(tab.javascript("undefined").await.unwrap(), "undefined");
    let err = tab.javascript("null.x").await.unwrap_err().to_string();
    assert!(err.contains("TypeError"), "{err}");

    assert_eq!(
        tab.page_text(1000).await.unwrap(),
        "Main part\n\nOnly this."
    );

    session.close().await;
}

#[tokio::test]
async fn form_input_sets_selects_checkboxes_and_fields_by_ref() {
    let addr = spawn_devtools_site().await;
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&format!("http://{addr}/")).await.unwrap();
    let ref_of = |line: &str| line.split(['[', ']']).nth(1).unwrap().to_string();

    let tree = tab.read_page(true, None, 15).await.unwrap();
    let find = |role: &str| ref_of(tree.iter().find(|l| l.starts_with(role)).unwrap());
    let (color, agree, name) = (find("- combobox"), find("- checkbox"), find("- textbox"));

    assert_eq!(
        tab.form_input(&color, &serde_json::json!("Green"))
            .await
            .unwrap(),
        "selected Green"
    );
    assert_eq!(
        tab.form_input(&agree, &serde_json::json!(true))
            .await
            .unwrap(),
        "checked"
    );
    assert_eq!(
        tab.form_input(&name, &serde_json::json!("Grüße"))
            .await
            .unwrap(),
        "set value"
    );
    let err = tab
        .form_input(&color, &serde_json::json!("Blue"))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("no option matches \"Blue\"; options: Red, Green"),
        "{err}"
    );

    assert_eq!(
        tab.javascript(
            "[document.getElementById('color').value, document.getElementById('agree').checked, \
             document.getElementById('name').value, window.changes.join()]"
        )
        .await
        .unwrap(),
        r#"["g",true,"Grüße","color,agree,name"]"#
    );
    session.close().await;
}

#[tokio::test]
async fn viewport_and_color_scheme_can_be_emulated() {
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&data_url(
        "<html><head><meta name=\"viewport\" content=\"width=device-width\"></head>\
         <body>x</body></html>",
    ))
    .await
    .unwrap();

    tab.set_viewport(375, 812, true).await.unwrap();
    tab.set_color_scheme(Some("dark")).await.unwrap();
    tab.page().reload().await.unwrap();
    assert_eq!(
        tab.javascript(
            "[innerWidth, innerHeight, navigator.maxTouchPoints, /Android/.test(navigator.userAgent), \
             matchMedia('(prefers-color-scheme: dark)').matches]"
        )
        .await
        .unwrap(),
        "[375,812,5,true,true]"
    );

    tab.set_viewport(1280, 800, false).await.unwrap();
    tab.set_color_scheme(None).await.unwrap();
    tab.page().reload().await.unwrap();
    assert_eq!(
        tab.javascript("[innerWidth, /Android/.test(navigator.userAgent)]")
            .await
            .unwrap(),
        "[1280,false]"
    );
    session.close().await;
}

/// Held keys and buttons: a page sees a key go down, stay down and come up
/// with real time in between, keys held across other presses, and a mouse
/// button held through a move.
#[tokio::test]
async fn keys_and_buttons_can_be_held() {
    use super::Button;
    use chromiumoxide::layout::Point;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&data_url(
        "<html><body style=\"margin:0;height:100vh\"><script>\
         window.log = [];\
         for (const t of ['keydown', 'keyup']) {\
           document.addEventListener(t, (e) => window.log.push([t, e.code, e.repeat, Math.round(performance.now())]));\
         }\
         for (const t of ['mousedown', 'mousemove', 'mouseup']) {\
           document.addEventListener(t, (e) => window.log.push([t, e.buttons, e.clientX, e.clientY]));\
         }\
         </script></body></html>",
    ))
    .await
    .unwrap();
    let log = || async {
        let json = tab
            .javascript("JSON.stringify(window.log.splice(0))")
            .await
            .unwrap();
        serde_json::from_str::<Vec<Vec<serde_json::Value>>>(&json).unwrap()
    };

    // Held for real time: up comes ~300 ms after down.
    tab.hold_keys("w", std::time::Duration::from_millis(300))
        .await
        .unwrap();
    let events = log().await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[0][0..2],
        [serde_json::json!("keydown"), serde_json::json!("KeyW")]
    );
    assert_eq!(
        events[1][0..2],
        [serde_json::json!("keyup"), serde_json::json!("KeyW")]
    );
    let held = events[1][3].as_f64().unwrap() - events[0][3].as_f64().unwrap();
    assert!((280.0..600.0).contains(&held), "held for {held} ms");

    // Held across another key: walk while jumping.
    tab.key_down("w").await.unwrap();
    tab.press_keys("space", 1).await.unwrap();
    assert_eq!(tab.held_inputs(), ["w"]);
    tab.key_up("W").await.unwrap();
    assert!(tab.held_inputs().is_empty(), "released by code, any case");
    let order: Vec<String> = log()
        .await
        .iter()
        .map(|e| format!("{}:{}", e[0].as_str().unwrap(), e[1].as_str().unwrap()))
        .collect();
    assert_eq!(
        order,
        ["keydown:KeyW", "keydown:Space", "keyup:Space", "keyup:KeyW"]
    );
    assert!(tab.key_down("nosuchkey").await.is_err());

    // A button held through a move, released where the mouse is.
    tab.mouse_down(Some(Point { x: 100.0, y: 100.0 }), Button::Left)
        .await
        .unwrap();
    tab.hover_point(Point { x: 300.0, y: 200.0 }).await.unwrap();
    assert_eq!(tab.held_inputs(), ["left mouse button"]);
    tab.mouse_up(None, Button::Left).await.unwrap();
    assert!(tab.held_inputs().is_empty());
    let events = log().await;
    let pressed = events.iter().find(|e| e[0] == "mousedown").unwrap();
    assert_eq!(
        pressed[1..],
        [
            serde_json::json!(1),
            serde_json::json!(100),
            serde_json::json!(100)
        ]
    );
    let moved = events.iter().rfind(|e| e[0] == "mousemove").unwrap();
    assert_eq!(
        moved[1..],
        [
            serde_json::json!(1),
            serde_json::json!(300),
            serde_json::json!(200)
        ]
    );
    let released = events.iter().find(|e| e[0] == "mouseup").unwrap();
    assert_eq!(
        released[1..],
        [
            serde_json::json!(0),
            serde_json::json!(300),
            serde_json::json!(200)
        ]
    );

    session.close().await;
}

/// Where the red box is in cell `i` of a contact sheet and its size: the
/// mean x of its red pixels and their bounding box, in cell pixels.
#[cfg(test)]
fn red_box_in_cell(
    sheet: &image::RgbaImage,
    rec: &super::Recording,
    i: usize,
) -> Option<(f64, u32, u32)> {
    let (cx, cy, cw, ch) = rec.cell_rect(i);
    let (mut sum, mut n) = (0u64, 0u64);
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in cy..cy + ch {
        for x in cx..cx + cw {
            let p = sheet.get_pixel(x, y);
            if p[0] > 200 && p[1] < 60 && p[2] < 60 {
                sum += (x - cx) as u64;
                n += 1;
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    (n > 0).then(|| (sum as f64 / n as f64, x1 - x0 + 1, y1 - y0 + 1))
}

/// A page with a red box sliding across at `top`, wrapping around.
#[cfg(test)]
fn sliding_box_url(top: u32) -> String {
    data_url(&format!(
        "<html><body style=\"margin:0;background:#fff\">\
         <div id=\"box\" style=\"position:absolute;top:{top}px;left:0;width:120px;height:120px;background:#f00\"></div>\
         <script>\
         const box = document.getElementById('box');\
         const step = (t) => {{ box.style.left = (40 + (t * 0.9) % 1100) + 'px'; requestAnimationFrame(step); }};\
         requestAnimationFrame(step);\
         </script></body></html>"
    ))
}

/// A recording turns the next second of the page into one contact sheet:
/// frames spread evenly over the time, each labelled with its offset, the
/// moving box in a different place in each — also where the viewport extends
/// past headless Chrome's default 800×600 window. A page that does not
/// repaint sends few frames; the sheet repeats the last one and says so.
#[tokio::test]
async fn recording_makes_a_contact_sheet_of_the_motion() {
    use std::time::Duration;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&sliding_box_url(300)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rec = tab.record(Duration::from_secs(1), 9, None).await.unwrap();
    let rate = rec.received;
    assert_eq!((rec.columns, rec.rows), (3, 3));
    assert_eq!(png_size(&rec.png), (rec.width, rec.height));
    assert!(rec.width <= super::MAX_SCREENSHOT_EDGE && rec.height <= super::MAX_SCREENSHOT_EDGE);
    let at: Vec<f64> = rec.frames.iter().map(|f| f.at).collect();
    assert_eq!(at, [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0]);
    // About 60 frames a second. A loaded machine paints fewer and may skip
    // a cell, which then repeats the one before; every fresh cell shows the
    // box somewhere new.
    assert!(
        rec.received >= 9,
        "only {} frames for a moving page",
        rec.received
    );
    let fresh = rec.frames[1..].iter().filter(|f| !f.repeated).count();
    assert!(fresh >= 6, "{:?}", rec.frames);
    let sheet = image::load_from_memory(&rec.png).unwrap().to_rgba8();
    let boxes: Vec<(f64, u32, u32)> = (0..9)
        .map(|i| red_box_in_cell(&sheet, &rec, i).unwrap_or_else(|| panic!("no box in cell {i}")))
        .collect();
    // Square in every cell: frames are cut to the viewport, not stretched.
    for (_, w, h) in &boxes {
        assert!(w.abs_diff(*h) <= 2, "{boxes:?}");
    }
    let xs: Vec<f64> = boxes.iter().map(|b| b.0).collect();
    for i in 1..9 {
        let moved = (xs[i] - xs[i - 1]).abs() > 2.0;
        assert_eq!(
            moved, !rec.frames[i].repeated,
            "cell {i}: {xs:?} {:?}",
            rec.frames
        );
    }

    // A viewport taller than the window: the box below 800 px still records
    // at the full rate (outside the window, only a fraction of the frames
    // would come).
    tab.set_viewport(1000, 1100, false).await.unwrap();
    tab.navigate(&sliding_box_url(950)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rec = tab.record(Duration::from_secs(1), 4, None).await.unwrap();
    assert!(
        rec.received * 3 >= rate * 2,
        "{} frames, {rate} in the default viewport",
        rec.received
    );

    // A window too small for the viewport is fitted to it before recording.
    tab.set_viewport(1280, 800, false).await.unwrap();
    tab.navigate(&sliding_box_url(600)).await.unwrap();
    let window = tab
        .page()
        .execute(chromiumoxide::cdp::browser_protocol::browser::GetWindowForTargetParams::default())
        .await
        .unwrap()
        .result;
    tab.page()
        .execute(
            chromiumoxide::cdp::browser_protocol::browser::SetWindowBoundsParams::new(
                window.window_id,
                chromiumoxide::cdp::browser_protocol::browser::Bounds::builder()
                    .width(1280)
                    .height(600)
                    .build(),
            ),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rec = tab.record(Duration::from_secs(1), 4, None).await.unwrap();
    assert!(!rec.frames[3].repeated, "{:?}", rec.frames);
    let sheet = image::load_from_memory(&rec.png).unwrap().to_rgba8();
    let (_, w, h) = red_box_in_cell(&sheet, &rec, 3).expect("the box low in the viewport");
    assert!(w.abs_diff(h) <= 2, "{w}×{h}");

    // A still page: every cell looks like the one before.
    tab.javascript("document.getElementById('box').remove(); 0")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let rec = tab
        .record(Duration::from_millis(500), 4, Some(0.25))
        .await
        .unwrap();
    assert_eq!((rec.columns, rec.rows), (2, 2));
    assert_eq!(rec.cell_rect(0).2, 320, "a quarter of 1280 px");
    assert!(
        rec.frames[1..].iter().all(|f| f.repeated),
        "{:?}",
        rec.frames
    );

    session.close().await;
}

/// A live view gets the page as soon as it subscribes — a still page paints
/// nothing, so the first frame is captured — then every repaint, each frame
/// within the size the viewer asked for and with the metadata that maps it
/// back to CSS pixels. The screencast runs only while someone watches.
#[tokio::test]
async fn live_frames_follow_the_page_while_watched() {
    use std::time::Duration;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&data_url(
        "<html><body style=\"background:#00f\"></body></html>",
    ))
    .await
    .unwrap();

    let mut live = tab.live_frames((640, 400));
    let first = tokio::time::timeout(Duration::from_secs(5), live.next())
        .await
        .expect("a still page shows up without a repaint")
        .expect("frames keep coming while watched");
    let img = image::load_from_memory(&first.jpeg).unwrap().to_rgba8();
    assert!(
        img.width() <= 640 && img.height() <= 400,
        "{:?}",
        img.dimensions()
    );
    let center = img.get_pixel(img.width() / 2, img.height() / 2);
    assert!(center[2] > 200 && center[0] < 60, "{center:?}");
    assert_eq!(first.metadata.device_width.round(), 1280.0);
    assert_eq!(first.metadata.device_height.round(), 800.0);
    assert!(tab.is_screencasting());

    // A repaint arrives as a new frame.
    tab.navigate(&sliding_box_url(300)).await.unwrap();
    let moving = tokio::time::timeout(Duration::from_secs(5), live.next())
        .await
        .unwrap()
        .unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &moving));

    drop(live);
    let stopped = async {
        while tab.is_screencasting() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), stopped)
        .await
        .expect("the screencast stops when nobody watches");

    session.close().await;
}

/// A recording and a live view share the tab's one screencast: recording
/// while the panel watches still collects the motion, and the panel keeps
/// getting frames afterwards.
#[tokio::test]
async fn recording_works_while_a_live_view_watches() {
    use std::time::Duration;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&sliding_box_url(300)).await.unwrap();
    let mut live = tab.live_frames((1280, 800));
    tokio::time::timeout(Duration::from_secs(5), live.next())
        .await
        .unwrap()
        .unwrap();

    let rec = tab.record(Duration::from_secs(1), 4, None).await.unwrap();
    assert!(rec.received >= 9, "only {} frames", rec.received);
    assert!(
        rec.frames[1..].iter().any(|f| !f.repeated),
        "{:?}",
        rec.frames
    );

    for _ in 0..3 {
        tokio::time::timeout(Duration::from_secs(5), live.next())
            .await
            .expect("the live view keeps getting frames after the recording")
            .unwrap();
    }
    assert!(tab.is_screencasting());

    session.close().await;
}

/// The session keeps a watchable list of its tabs for a browser panel:
/// address and title follow navigations, a page shows as loading until it
/// has, and tabs the page opens or closes come and go without a tool call.
#[tokio::test]
async fn tab_state_follows_navigations_and_popups() {
    use axum::response::Html;
    use axum::{Router, routing::get};
    use std::time::Duration;

    async fn slow() -> Html<&'static str> {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        Html("<html><head><title>Slow</title></head><body>done</body></html>")
    }
    let app = Router::new().route("/slow", get(slow));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slow_addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let addr = spawn_form_site().await;

    let session = std::sync::Arc::new(
        BrowserSession::open(BrowserLaunchConfig::default(), "test")
            .await
            .unwrap(),
    );
    let mut state = session.watch_tabs();
    type Pred = Box<dyn Fn(&[super::TabInfo]) -> bool + Send + Sync>;
    let until = |state: &mut tokio::sync::watch::Receiver<Vec<super::TabInfo>>,
                 what: &'static str,
                 pred: Pred| {
        let mut state = state.clone();
        let last = state.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(10), state.wait_for(|tabs| pred(tabs)))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}: {:?}", *last.borrow()))
                .unwrap()
                .clone()
        }
    };

    let tab = session.active_tab().unwrap();
    tab.navigate(&format!("http://{addr}/")).await.unwrap();
    let tabs = until(
        &mut state,
        "the title",
        Box::new(|tabs| tabs.len() == 1 && tabs[0].title == "Login Demo"),
    )
    .await;
    assert_eq!(tabs[0].url, format!("http://{addr}/"));
    assert!(tabs[0].active);

    // Loading while the server takes its time, done once it answered.
    let slow_url = format!("http://{slow_addr}/slow");
    tab.javascript(&format!("location.href = '{slow_url}'; 0"))
        .await
        .unwrap();
    until(&mut state, "loading", Box::new(|tabs| tabs[0].loading)).await;
    until(
        &mut state,
        "the slow page",
        Box::new(|tabs| !tabs[0].loading && tabs[0].title == "Slow"),
    )
    .await;

    // A popup joins the list on its own, behind the active tab.
    tab.javascript(&format!("window.open('http://{addr}/'); 0"))
        .await
        .unwrap();
    let tabs = until(&mut state, "the popup", Box::new(|tabs| tabs.len() == 2)).await;
    assert!(tabs[0].active && !tabs[1].active, "{tabs:?}");

    // Closed by the page, it leaves again.
    let popup = session.tab(Some(&tabs[1].id)).unwrap();
    popup.javascript("window.close(); 0").await.ok();
    until(
        &mut state,
        "the popup to close",
        Box::new(|tabs| tabs.len() == 1),
    )
    .await;

    // Switching tabs shows in the state too.
    let second = session.create_tab(false).await.unwrap();
    session.select_tab(second.id()).unwrap();
    until(
        &mut state,
        "the new active tab",
        Box::new(|tabs| tabs.len() == 2 && tabs[1].active && !tabs[0].active),
    )
    .await;

    session.close().await;
}

/// Where the agent presses the mouse is announced, so a panel can show it.
#[tokio::test]
async fn agent_presses_are_announced() {
    use super::Button;
    use chromiumoxide::layout::Point;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    let mut presses = tab.watch_agent_presses();
    tab.click_point(Point { x: 120.0, y: 80.0 }, Button::Left, 2, 0)
        .await
        .unwrap();
    let at = presses.try_recv().expect("the press was announced");
    assert_eq!((at.x, at.y), (120.0, 80.0));
    assert!(presses.try_recv().is_err(), "a double click is one press");

    session.close().await;
}

/// A throwaway browser someone is watching survives the end of the turn and
/// closes when the last viewer lets go — unless the agent used it again
/// meanwhile, in which case the next turn end decides.
#[tokio::test]
async fn viewed_throwaway_browsers_outlive_the_turn() {
    use std::sync::Arc;
    use std::time::Duration;

    let open = || async {
        Arc::new(
            BrowserSession::open(BrowserLaunchConfig::default(), "default")
                .await
                .unwrap(),
        )
    };
    let manager = Arc::new(BrowserSessionManager::new(4));
    let mut changes = manager.watch_changes();
    manager.register(open().await, "default");
    assert!(changes.has_changed().unwrap(), "registering is a change");
    changes.mark_unchanged();

    let viewer = manager.view("default").expect("a tracked browser");
    let second = manager.view("default").unwrap();
    manager.close_ephemeral().await;
    assert_eq!(manager.list().len(), 1, "watched: kept past the turn");
    drop(viewer);
    assert_eq!(
        manager.list().len(),
        1,
        "still watched by the second viewer"
    );
    drop(second);
    let closed = async {
        while !manager.list().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), closed)
        .await
        .expect("closed once nobody watches");
    assert!(changes.has_changed().unwrap(), "closing is a change");

    // Used by the agent again after the turn: releasing the view keeps it.
    manager.register(open().await, "default");
    let viewer = manager.view("default").unwrap();
    manager.close_ephemeral().await;
    assert!(manager.get_by_label("default").is_some());
    drop(viewer);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(manager.list().len(), 1, "in use again, not orphaned");
    manager.close_ephemeral().await;
    assert!(
        manager.list().is_empty(),
        "unwatched: closed at the turn end"
    );
}

/// The user can take over a browser; the next look by the agent learns that
/// the user had it, once.
#[tokio::test]
async fn the_user_can_take_control() {
    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    assert!(!session.user_in_control());
    assert!(!session.take_user_interlude());
    session.set_user_control(true);
    assert!(session.user_in_control());
    session.set_user_control(false);
    assert!(!session.user_in_control());
    assert!(session.take_user_interlude());
    assert!(!session.take_user_interlude(), "reported once");
    session.close().await;
}

/// What a person does in a browser panel reaches the page as is: clicks,
/// typing (shifted characters and text a key table does not know), editing
/// commands, the wheel, and the address bar's navigation.
#[tokio::test]
async fn a_person_at_the_panel_clicks_types_and_scrolls() {
    use super::{Button, UserInput};
    use chromiumoxide::layout::Point;

    let session = BrowserSession::open(BrowserLaunchConfig::default(), "test")
        .await
        .unwrap();
    let tab = session.active_tab().unwrap();
    tab.navigate(&input_lab_url()).await.unwrap();
    let field = Point { x: 150.0, y: 110.0 };
    for input in [
        UserInput::MouseMove {
            at: field,
            buttons: 0,
            modifiers: 0,
        },
        UserInput::MouseDown {
            at: field,
            button: Button::Left,
            click_count: 1,
            buttons: 1,
            modifiers: 0,
        },
        UserInput::MouseUp {
            at: field,
            button: Button::Left,
            click_count: 1,
            buttons: 0,
            modifiers: 0,
        },
        UserInput::KeyDown {
            key: "a".into(),
            text: Some("a".into()),
            modifiers: 0,
            commands: vec![],
        },
        UserInput::KeyUp {
            key: "a".into(),
            modifiers: 0,
        },
        UserInput::KeyDown {
            key: "1".into(),
            text: Some("!".into()),
            modifiers: 8,
            commands: vec![],
        },
        UserInput::KeyUp {
            key: "1".into(),
            modifiers: 8,
        },
        UserInput::InsertText("ü".into()),
    ] {
        tab.user_input(input).await.unwrap();
    }
    assert_eq!(
        tab.javascript("document.getElementById('f').value")
            .await
            .unwrap(),
        "a!ü"
    );

    tab.user_input(UserInput::KeyDown {
        key: "a".into(),
        text: None,
        modifiers: 4,
        commands: vec!["selectAll".into()],
    })
    .await
    .unwrap();
    tab.user_input(UserInput::KeyDown {
        key: "Backspace".into(),
        text: None,
        modifiers: 0,
        commands: vec![],
    })
    .await
    .unwrap();
    assert_eq!(
        tab.javascript("document.getElementById('f').value")
            .await
            .unwrap(),
        ""
    );

    tab.user_input(UserInput::Wheel {
        at: Point { x: 700.0, y: 150.0 },
        dx: 0.0,
        dy: 120.0,
        modifiers: 0,
    })
    .await
    .unwrap();
    let scrolled = async {
        while tab
            .javascript("document.getElementById('scroller').scrollTop")
            .await
            .unwrap()
            == "0"
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), scrolled)
        .await
        .expect("the wheel scrolls the box under it");

    let other = data_url("<title>Other</title>other");
    tab.user_input(UserInput::Navigate(other)).await.unwrap();
    assert_eq!(tab.javascript("document.title").await.unwrap(), "Other");
    tab.user_input(UserInput::History(-1)).await.unwrap();
    tab.settle().await;
    assert!(
        tab.javascript("!!document.getElementById('f')")
            .await
            .unwrap()
            == "true"
    );
    tab.user_input(UserInput::Reload).await.unwrap();

    session.close().await;
}
