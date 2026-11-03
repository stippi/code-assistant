//! OAuth login against an in-process authorization server: discovery,
//! dynamic client registration, the authorization-code exchange and the
//! refresh, checking how the client authenticates at the token endpoint for
//! each `token_endpoint_auth_methods_supported` the server advertises.

use crate::auth::{AuthorizationOutcome, OAuthAuthorizer};
use crate::client::{authenticate_http_server, stored_login_manager};
use crate::config::McpServerConfig;
use base64::Engine;
use rmcp::transport::auth::{CredentialStore, InMemoryCredentialStore};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One request the token endpoint received.
#[derive(Debug, Clone)]
struct TokenRequest {
    /// The `Authorization` header, if any.
    authorization: Option<String>,
    /// The form-encoded body.
    form: HashMap<String, String>,
}

impl TokenRequest {
    /// The `client_id:client_secret` pair of an HTTP Basic header.
    fn basic_credentials(&self) -> Option<(String, String)> {
        let encoded = self.authorization.as_deref()?.strip_prefix("Basic ")?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        let (id, secret) = decoded.split_once(':')?;
        Some((id.to_string(), secret.to_string()))
    }
}

/// How the mock authorization server behaves.
#[derive(Clone)]
struct AuthServerSpec {
    /// `token_endpoint_auth_methods_supported`; `None` omits the field.
    auth_methods: Option<Vec<&'static str>>,
    /// The secret dynamic client registration hands out, if any.
    issued_secret: Option<&'static str>,
}

const CLIENT_ID: &str = "registered-client";

struct AuthServer {
    base: String,
    requests: Arc<Mutex<Vec<TokenRequest>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl AuthServer {
    fn token_requests(&self) -> Vec<TokenRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn config(&self) -> McpServerConfig {
        serde_json::from_value(json!({ "url": format!("{}/mcp", self.base) })).unwrap()
    }
}

/// Decide whether the token endpoint accepts `request`, the way a strict
/// authorization server would for the methods it advertises.
fn client_authenticated(spec: &AuthServerSpec, request: &TokenRequest) -> Result<(), String> {
    let methods = spec
        .auth_methods
        .clone()
        .unwrap_or_else(|| vec!["client_secret_basic"]);
    let body_id = request.form.get("client_id");
    let body_secret = request.form.get("client_secret");
    let basic = request.basic_credentials();

    if methods.contains(&"client_secret_basic")
        && let Some((id, secret)) = &basic
    {
        return if id == CLIENT_ID && Some(secret.as_str()) == spec.issued_secret {
            Ok(())
        } else {
            Err("invalid_client: bad basic credentials".into())
        };
    }
    if methods.contains(&"client_secret_post") && body_secret.is_some() {
        return if body_id.map(String::as_str) == Some(CLIENT_ID)
            && body_secret.map(String::as_str) == spec.issued_secret
        {
            Ok(())
        } else {
            Err("invalid_client: bad client_secret".into())
        };
    }
    if methods.contains(&"none") {
        // A public client: identified by the body's client_id, and a server
        // that only knows `none` never looks at an Authorization header.
        return match body_id {
            Some(id) if id == CLIENT_ID => Ok(()),
            Some(_) => Err("invalid_client: unknown client_id".into()),
            None => Err("invalid_client: client_id is required".into()),
        };
    }
    Err("invalid_client: client authentication required".into())
}

async fn spawn_auth_server(spec: AuthServerSpec) -> AuthServer {
    use axum::extract::{Form, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));

    #[derive(Clone)]
    struct AppState {
        base: String,
        spec: AuthServerSpec,
        requests: Arc<Mutex<Vec<TokenRequest>>>,
    }
    let state = AppState {
        base: base.clone(),
        spec,
        requests: requests.clone(),
    };

    async fn resource_metadata(State(state): State<AppState>) -> impl IntoResponse {
        axum::Json(json!({
            "resource": format!("{}/mcp", state.base),
            "authorization_servers": [state.base],
        }))
    }
    async fn server_metadata(State(state): State<AppState>) -> impl IntoResponse {
        let base = &state.base;
        let mut metadata = json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "registration_endpoint": format!("{base}/register"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
        });
        if let Some(methods) = &state.spec.auth_methods {
            metadata["token_endpoint_auth_methods_supported"] = json!(methods);
        }
        axum::Json(metadata)
    }
    async fn register(
        State(state): State<AppState>,
        axum::Json(request): axum::Json<Value>,
    ) -> impl IntoResponse {
        let mut response = json!({
            "client_id": CLIENT_ID,
            "redirect_uris": request["redirect_uris"],
            "token_endpoint_auth_method": request["token_endpoint_auth_method"],
        });
        if let Some(secret) = state.spec.issued_secret {
            response["client_secret"] = json!(secret);
        }
        (StatusCode::CREATED, axum::Json(response))
    }
    async fn token(
        State(state): State<AppState>,
        headers: HeaderMap,
        Form(form): Form<HashMap<String, String>>,
    ) -> axum::response::Response {
        let request = TokenRequest {
            authorization: headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            form,
        };
        state.requests.lock().unwrap().push(request.clone());
        if let Err(description) = client_authenticated(&state.spec, &request) {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({
                    "error": "invalid_client",
                    "error_description": description,
                })),
            )
                .into_response();
        }
        axum::Json(json!({
            "access_token": "access",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "refresh",
        }))
        .into_response()
    }

    let router = axum::Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(server_metadata),
        )
        .route("/register", post(register))
        .route("/token", post(token))
        .with_state(state);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    AuthServer {
        base,
        requests,
        _task: task,
    }
}

/// Plays the browser: approves the authorization URL right away and hands
/// back a code with the URL's `state`.
struct InstantApproval;

#[async_trait::async_trait]
impl OAuthAuthorizer for InstantApproval {
    fn redirect_uri(&self) -> String {
        "http://127.0.0.1:1/callback".to_string()
    }

    async fn authorize(&self, authorization_url: String) -> anyhow::Result<AuthorizationOutcome> {
        let url = reqwest::Url::parse(&authorization_url)?;
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .ok_or_else(|| anyhow::anyhow!("authorization URL without state"))?;
        Ok(AuthorizationOutcome {
            code: "the-code".to_string(),
            state,
            issuer: None,
        })
    }
}

/// Log in against `server`, returning the store holding the tokens.
async fn log_in(server: &AuthServer) -> anyhow::Result<Arc<dyn CredentialStore>> {
    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
    authenticate_http_server(
        "oauth-test",
        &server.config(),
        store.clone(),
        &InstantApproval,
        "code-assistant",
    )
    .await?;
    Ok(store)
}

/// The single token request of a login (the authorization-code exchange).
fn code_exchange(server: &AuthServer) -> TokenRequest {
    let requests = server.token_requests();
    assert_eq!(
        requests.len(),
        1,
        "one token request expected: {requests:?}"
    );
    let request = requests.into_iter().next().unwrap();
    assert_eq!(
        request.form.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert!(
        request.form.contains_key("code_verifier"),
        "PKCE verifier sent"
    );
    request
}

/// Refresh the stored login the way a later connect does.
async fn refresh(server: &AuthServer, store: Arc<dyn CredentialStore>) -> TokenRequest {
    let manager = stored_login_manager("oauth-test", &format!("{}/mcp", server.base), store)
        .await
        .unwrap()
        .expect("the login was stored");
    manager.refresh_token().await.expect("refresh must succeed");
    let request = server.token_requests().pop().unwrap();
    assert_eq!(
        request.form.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    request
}

/// A public client: `client_id` in the body, no secret anywhere, no Basic.
fn assert_public_client(request: &TokenRequest) {
    assert_eq!(
        request.form.get("client_id").map(String::as_str),
        Some(CLIENT_ID),
        "{request:?}"
    );
    assert!(!request.form.contains_key("client_secret"), "{request:?}");
    assert_eq!(request.authorization, None, "{request:?}");
}

/// `client_secret_basic`: the client in the Basic header, nothing in the body.
fn assert_basic_auth(request: &TokenRequest, secret: &str) {
    assert_eq!(
        request.basic_credentials(),
        Some((CLIENT_ID.to_string(), secret.to_string())),
        "{request:?}"
    );
    assert!(!request.form.contains_key("client_secret"), "{request:?}");
    assert!(!request.form.contains_key("client_id"), "{request:?}");
}

#[tokio::test]
async fn public_client_without_secret_authenticates_with_client_id_in_body() {
    let server = spawn_auth_server(AuthServerSpec {
        auth_methods: Some(vec!["none"]),
        issued_secret: None,
    })
    .await;

    let store = log_in(&server).await.expect("login must succeed");
    assert_public_client(&code_exchange(&server));
    assert_public_client(&refresh(&server, store).await);
}

/// The regression: registration hands out a secret although the server
/// only accepts public clients. Sending it as HTTP Basic left the body
/// without `client_id` (`invalid_client: client_id is required`).
#[tokio::test]
async fn public_only_server_issuing_a_secret_still_gets_a_public_client() {
    let server = spawn_auth_server(AuthServerSpec {
        auth_methods: Some(vec!["none"]),
        issued_secret: Some("unwanted-secret"),
    })
    .await;

    let store = log_in(&server).await.expect("login must succeed");
    assert_public_client(&code_exchange(&server));
    assert_public_client(&refresh(&server, store).await);
}

#[tokio::test]
async fn client_secret_basic_sends_the_secret_as_basic_auth() {
    let server = spawn_auth_server(AuthServerSpec {
        auth_methods: Some(vec!["client_secret_basic"]),
        issued_secret: Some("basic-secret"),
    })
    .await;

    log_in(&server).await.expect("login must succeed");
    assert_basic_auth(&code_exchange(&server), "basic-secret");
}

#[tokio::test]
async fn client_secret_post_sends_the_secret_in_the_body() {
    let server = spawn_auth_server(AuthServerSpec {
        auth_methods: Some(vec!["client_secret_post"]),
        issued_secret: Some("post-secret"),
    })
    .await;

    log_in(&server).await.expect("login must succeed");
    let request = code_exchange(&server);
    assert_eq!(
        request.form.get("client_id").map(String::as_str),
        Some(CLIENT_ID)
    );
    assert_eq!(
        request.form.get("client_secret").map(String::as_str),
        Some("post-secret")
    );
    assert_eq!(request.authorization, None);
}

/// Without a method list the server defaults to `client_secret_basic`
/// (RFC 8414), so an issued secret keeps going out as Basic.
#[tokio::test]
async fn unadvertised_methods_keep_basic_auth() {
    let server = spawn_auth_server(AuthServerSpec {
        auth_methods: None,
        issued_secret: Some("basic-secret"),
    })
    .await;

    log_in(&server).await.expect("login must succeed");
    assert_basic_auth(&code_exchange(&server), "basic-secret");
}

/// A server accepting public and confidential clients keeps the issued
/// secret: only a `none`-only server turns the client public.
#[tokio::test]
async fn none_next_to_a_secret_method_keeps_the_secret() {
    let server = spawn_auth_server(AuthServerSpec {
        auth_methods: Some(vec!["none", "client_secret_basic"]),
        issued_secret: Some("basic-secret"),
    })
    .await;

    log_in(&server).await.expect("login must succeed");
    assert_basic_auth(&code_exchange(&server), "basic-secret");
}
