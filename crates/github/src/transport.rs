//! The seam between the client and the network: one request out, one
//! answer back. The client builds requests and interprets answers; a
//! transport only moves bytes.

use async_trait::async_trait;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Header names are kept lower-case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: &str, value: impl Into<String>) {
        let name = name.to_ascii_lowercase();
        self.0.retain(|(existing, _)| *existing != name);
        self.0.push((name, value.into()));
    }

    pub fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.insert(name, value);
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Headers,
    pub body: Option<Vec<u8>>,
    /// The body is read up to this many bytes; the rest is dropped and the
    /// response marked truncated.
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
    /// The body was longer than `Request::max_body_bytes`.
    pub truncated: bool,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The request never got an answer: connection, TLS or I/O failure.
#[derive(Debug, Clone)]
pub struct TransportError(pub String);

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, request: Request) -> Result<Response, TransportError>;
}

/// The network transport. Deadlines are the client's: it bounds each
/// exchange, headers and body together.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to build GitHub HTTP client, using defaults: {e}");
                reqwest::Client::new()
            });
        Self { client }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Patch => reqwest::Method::PATCH,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in request.headers.iter() {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        // Without the URL: query strings carry shas and branch names.
        let fail = |e: reqwest::Error| TransportError(e.without_url().to_string());
        let mut response = builder.send().await.map_err(fail)?;
        let status = response.status().as_u16();
        let mut headers = Headers::new();
        for (name, value) in response.headers() {
            if let Ok(value) = value.to_str() {
                headers.insert(name.as_str(), value);
            }
        }
        let mut body = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = response.chunk().await.map_err(fail)? {
            let room = request.max_body_bytes.saturating_sub(body.len());
            if chunk.len() > room {
                body.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status,
            headers,
            body,
            truncated,
        })
    }
}
