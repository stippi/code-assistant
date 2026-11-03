//! A scripted transport for tests: answers GraphQL requests by operation
//! name (and optionally variables) and REST requests by method and path,
//! and records every request.

use crate::auth::StaticTokenSource;
use crate::client::Client;
use crate::transport::{Headers, Method, Request, Response, Transport, TransportError};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A scripted answer.
#[derive(Debug, Clone)]
pub struct FakeResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl FakeResponse {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    pub fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: Headers::new().with("content-type", "application/json"),
            body: body.to_string().into_bytes(),
        }
    }

    pub fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: Headers::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    /// A GraphQL answer with `data`.
    pub fn data(data: Value) -> Self {
        Self::json(200, json!({ "data": data }))
    }

    /// A GraphQL answer with `errors` (and no data).
    pub fn errors(errors: Value) -> Self {
        Self::json(200, json!({ "data": null, "errors": errors }))
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name, value);
        self
    }
}

/// A request as the fake saw it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: Method,
    pub url: String,
    /// Relative to the API root, query included (`repos/o/n/pulls/1?x=y`),
    /// or `graphql`.
    pub path: String,
    pub headers: Headers,
    pub body: Option<Value>,
    /// The GraphQL operation name.
    pub operation: Option<String>,
    pub variables: Option<Value>,
    pub query: Option<String>,
}

#[derive(Debug, Clone)]
enum Matcher {
    GraphQl {
        operation: String,
        variables: Option<Value>,
    },
    Rest {
        method: Method,
        path: String,
    },
}

#[derive(Debug)]
struct Route {
    matcher: Matcher,
    response: FakeResponse,
    remaining: Option<usize>,
}

#[derive(Default)]
pub struct FakeTransport {
    routes: Mutex<Vec<Route>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

/// Registers one route; `respond` (or a shorthand) completes it.
pub struct RouteBuilder<'a> {
    fake: &'a FakeTransport,
    matcher: Matcher,
    remaining: Option<usize>,
}

impl RouteBuilder<'_> {
    /// Only requests whose variables include these (top-level keys, equal values).
    pub fn with_variables(mut self, variables: Value) -> Self {
        if let Matcher::GraphQl { variables: v, .. } = &mut self.matcher {
            *v = Some(variables);
        }
        self
    }

    /// Answers once, then gives way to later routes.
    pub fn once(self) -> Self {
        self.times(1)
    }

    pub fn times(mut self, times: usize) -> Self {
        self.remaining = Some(times);
        self
    }

    pub fn respond(self, response: FakeResponse) {
        self.fake.routes.lock().unwrap().push(Route {
            matcher: self.matcher,
            response,
            remaining: self.remaining,
        });
    }

    /// A GraphQL answer with `data`.
    pub fn data(self, data: Value) {
        self.respond(FakeResponse::data(data));
    }

    /// A 200 JSON answer.
    pub fn json(self, body: Value) {
        self.respond(FakeResponse::json(200, body));
    }
}

fn api_path(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.split_once('/').map_or("", |(_, path)| path);
    path.strip_prefix("api/v3/")
        .or_else(|| path.strip_prefix("api/"))
        .unwrap_or(path)
        .to_string()
}

fn includes(actual: &Value, wanted: &Value) -> bool {
    match (actual, wanted) {
        (Value::Object(actual), Value::Object(wanted)) => wanted
            .iter()
            .all(|(key, value)| actual.get(key).unwrap_or(&Value::Null) == value),
        _ => actual == wanted,
    }
}

impl FakeTransport {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn on_graphql(&self, operation: &str) -> RouteBuilder<'_> {
        RouteBuilder {
            fake: self,
            matcher: Matcher::GraphQl {
                operation: operation.to_string(),
                variables: None,
            },
            remaining: None,
        }
    }

    /// `path` is relative to the API root, query string included.
    pub fn on_rest(&self, method: Method, path: &str) -> RouteBuilder<'_> {
        RouteBuilder {
            fake: self,
            matcher: Matcher::Rest {
                method,
                path: path.to_string(),
            },
            remaining: None,
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// The GraphQL operations requested, in order.
    pub fn operations(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter_map(|r| r.operation)
            .collect()
    }

    /// A client over this transport with a fixed token.
    pub fn client(self: &Arc<Self>) -> Client {
        Client::with_parts(
            self.clone(),
            Arc::new(StaticTokenSource("test-token".into())),
        )
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        let path = api_path(&request.url);
        let body: Option<Value> = request
            .body
            .as_deref()
            .and_then(|b| serde_json::from_slice(b).ok());
        let graphql = path.ends_with("graphql");
        let field = |name: &str| {
            body.as_ref()
                .filter(|_| graphql)
                .and_then(|b| b.get(name))
                .cloned()
        };
        let recorded = RecordedRequest {
            method: request.method,
            url: request.url.clone(),
            path: path.clone(),
            headers: request.headers.clone(),
            operation: field("operationName").and_then(|v| v.as_str().map(str::to_string)),
            variables: field("variables"),
            query: field("query").and_then(|v| v.as_str().map(str::to_string)),
            body: body.clone(),
        };
        self.requests.lock().unwrap().push(recorded.clone());

        let mut routes = self.routes.lock().unwrap();
        let route = routes.iter_mut().find(|route| {
            route.remaining != Some(0)
                && match &route.matcher {
                    Matcher::GraphQl {
                        operation,
                        variables,
                    } => {
                        graphql
                            && recorded.operation.as_deref() == Some(operation.as_str())
                            && variables.as_ref().is_none_or(|wanted| {
                                includes(
                                    recorded.variables.as_ref().unwrap_or(&Value::Null),
                                    wanted,
                                )
                            })
                    }
                    Matcher::Rest { method, path: p } => {
                        !graphql && *method == request.method && *p == path
                    }
                }
        });
        let Some(route) = route else {
            return Err(TransportError(format!(
                "fake: no route for {} {} {}",
                request.method,
                path,
                recorded.operation.unwrap_or_default()
            )));
        };
        if let Some(remaining) = &mut route.remaining {
            *remaining -= 1;
        }
        let mut body = route.response.body.clone();
        let truncated = body.len() > request.max_body_bytes;
        body.truncate(request.max_body_bytes);
        Ok(Response {
            status: route.response.status,
            headers: route.response.headers.clone(),
            body,
            truncated,
        })
    }
}
