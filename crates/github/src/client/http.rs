//! Sending one request: token, slot, quota and pause checks, the deadline,
//! reading the answer, and turning refusals into errors.

use super::{Client, Priority};
use crate::error::{Error, GraphQlError, Result};
use crate::graphql::{self, Envelope, alias};
use crate::limits::{self, is_rate_limited, now_ms, system_time};
use crate::repo::{api_urls, is_github_host};
use crate::transport::{Headers, Method, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = "code-assistant";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Api {
    Rest,
    GraphQl,
}

impl Api {
    /// The quota a request spends.
    fn resource(self) -> &'static str {
        match self {
            Api::Rest => "core",
            Api::GraphQl => "graphql",
        }
    }
}

struct Call<'a> {
    host: &'a str,
    operation: &'static str,
    api: Api,
    method: Method,
    url: String,
    headers: Headers,
    body: Option<Vec<u8>>,
    timeout: Duration,
    max_body_bytes: usize,
    allow_reserve: Option<bool>,
}

/// A REST request relative to the API root (`repos/o/n/pulls/1?x=y`).
pub(crate) struct RestCall<'a> {
    host: &'a str,
    operation: &'static str,
    method: Method,
    path: String,
    body: Option<Value>,
    accept: &'static str,
    if_none_match: Option<String>,
    timeout: Duration,
    max_body_bytes: usize,
    allow_reserve: Option<bool>,
}

impl<'a> RestCall<'a> {
    pub fn new(host: &'a str, operation: &'static str, method: Method, path: String) -> Self {
        Self {
            host,
            operation,
            method,
            path,
            body: None,
            accept: "application/vnd.github+json",
            if_none_match: None,
            timeout: DEFAULT_TIMEOUT,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            allow_reserve: None,
        }
    }

    pub fn get(host: &'a str, operation: &'static str, path: String) -> Self {
        Self::new(host, operation, Method::Get, path)
    }

    pub fn json(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    pub fn accept(mut self, accept: &'static str) -> Self {
        self.accept = accept;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_body_bytes(mut self, max: usize) -> Self {
        self.max_body_bytes = max;
        self
    }

    pub fn allow_reserve(mut self) -> Self {
        self.allow_reserve = Some(true);
        self
    }
}

/// A REST read through the ETag cache.
pub(crate) struct Revalidated {
    pub headers: Headers,
    pub body: Vec<u8>,
    /// False when GitHub answered 304: the body is the stored one.
    pub changed: bool,
}

/// The messages of a REST error body: `message` and `errors[]`.
fn rest_error_messages(body: &[u8]) -> Vec<String> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Detail {
        Text(String),
        Fields {
            message: Option<String>,
            field: Option<String>,
            code: Option<String>,
        },
    }
    #[derive(Deserialize)]
    struct Body {
        message: Option<String>,
        #[serde(default)]
        errors: Vec<Detail>,
    }
    let Ok(body) = serde_json::from_slice::<Body>(body) else {
        return Vec::new();
    };
    body.message
        .into_iter()
        .chain(body.errors.into_iter().filter_map(|detail| match detail {
            Detail::Text(text) => Some(text),
            Detail::Fields {
                message: Some(message),
                ..
            } => Some(message),
            Detail::Fields {
                field: Some(field),
                code: Some(code),
                ..
            } => Some(format!("{field} {code}")),
            Detail::Fields { .. } => None,
        }))
        .collect()
}

fn graphql_failure(operation: &'static str, errors: Vec<GraphQlError>) -> Error {
    if errors.iter().all(|e| e.is_kind("NOT_FOUND")) {
        Error::NotFound { operation }
    } else {
        Error::GraphQl { operation, errors }
    }
}

impl Client {
    async fn execute(&self, call: Call<'_>) -> Result<(Response, Option<Envelope>)> {
        let host = call.host;
        if !is_github_host(host) {
            return Err(Error::Invalid(format!(
                "'{host}' is not a GitHub host name"
            )));
        }
        let allow_reserve = call
            .allow_reserve
            .unwrap_or(self.priority == Priority::Interactive);
        let resource = call.api.resource();
        let inner = &self.inner;
        // A refused token is dropped and asked for again, once.
        for attempt in 0..2 {
            let token = inner.tokens.get(host).await?;
            let permit = inner.slots.acquire().await.expect("slots are never closed");
            let lease = {
                let now = now_ms();
                let limits = inner.limits.lock().unwrap();
                let paused = |retry_at: u64| Error::Paused {
                    host: host.to_string(),
                    retry_at: system_time(retry_at),
                };
                limits
                    .quota
                    .admit(host, resource, allow_reserve, now)
                    .map_err(paused)?;
                limits
                    .pauses
                    .check(host, now, allow_reserve)
                    .map_err(paused)?
            };
            let mut headers = call.headers.clone();
            headers.insert("authorization", format!("Bearer {token}"));
            headers.insert("x-github-api-version", API_VERSION);
            headers.insert("user-agent", USER_AGENT);
            let request = Request {
                method: call.method,
                url: call.url.clone(),
                headers,
                body: call.body.clone(),
                max_body_bytes: call.max_body_bytes,
            };
            let failed = |detail: String| Error::Request {
                host: host.to_string(),
                operation: call.operation,
                detail,
            };
            // One deadline for headers and body: a host that stalls mid-body
            // must not hold a slot.
            let response =
                match tokio::time::timeout(call.timeout, inner.transport.send(request)).await {
                    Err(_) => {
                        return Err(failed(format!(
                            "no answer within {} s",
                            call.timeout.as_secs()
                        )));
                    }
                    Ok(Err(e)) => return Err(failed(e.0)),
                    Ok(Ok(response)) => response,
                };
            drop(permit);

            let envelope = if call.api == Api::GraphQl
                && (200..300).contains(&response.status)
                && !response.truncated
            {
                Some(
                    serde_json::from_slice::<Envelope>(&response.body)
                        .map_err(|e| Error::decode(call.operation, e))?,
                )
            } else {
                None
            };

            {
                let now = now_ms();
                let mut limits = inner.limits.lock().unwrap();
                limits.quota.observe(host, &response.headers);
                let body = if matches!(response.status, 403 | 429) {
                    response.text()
                } else {
                    String::new()
                };
                let errors = envelope.as_ref().map(|e| e.errors.as_slice());
                if is_rate_limited(response.status, &response.headers, errors, &body) {
                    let retry_at = limits::retry_at(&response.headers, now);
                    limits.pauses.record_rate_limit(host, lease, retry_at, now);
                    return Err(Error::RateLimited {
                        host: host.to_string(),
                        retry_at: retry_at.map(system_time),
                    });
                }
                if response.status != 401 {
                    limits.pauses.record_success(host, lease, now);
                }
            }

            if response.status == 401 {
                inner.tokens.invalidate(host).await;
                if attempt == 0 {
                    continue;
                }
                return Err(Error::Unauthorized {
                    host: host.to_string(),
                });
            }
            return Ok((response, envelope));
        }
        unreachable!("the loop returns on its second attempt")
    }

    /// A REST request. 2xx (and a 304 when revalidating) is an answer; a
    /// 404 is `NotFound`; anything else an `Api` error with GitHub's messages.
    pub(crate) async fn rest(&self, call: RestCall<'_>) -> Result<Response> {
        self.rest_with(call, None).await
    }

    async fn rest_with(
        &self,
        call: RestCall<'_>,
        if_none_match: Option<String>,
    ) -> Result<Response> {
        let operation = call.operation;
        let revalidating = if_none_match.is_some() || call.if_none_match.is_some();
        let mut headers = Headers::new().with("accept", call.accept);
        if let Some(etag) = if_none_match.or(call.if_none_match) {
            headers.insert("if-none-match", etag);
        }
        let body = call.body.map(|body| {
            headers.insert("content-type", "application/json");
            serde_json::to_vec(&body).expect("JSON values serialize")
        });
        let (response, _) = self
            .execute(Call {
                host: call.host,
                operation,
                api: Api::Rest,
                method: call.method,
                url: format!(
                    "{}/{}",
                    api_urls(call.host).rest,
                    call.path.trim_start_matches('/')
                ),
                headers,
                body,
                timeout: call.timeout,
                max_body_bytes: call.max_body_bytes,
                allow_reserve: call.allow_reserve,
            })
            .await?;
        match response.status {
            200..=299 => Ok(response),
            304 if revalidating => Ok(response),
            404 => Err(Error::NotFound { operation }),
            status => Err(Error::Api {
                operation,
                status,
                messages: rest_error_messages(&response.body),
            }),
        }
    }

    /// A REST GET answered from the ETag cache when GitHub says nothing changed.
    pub(crate) async fn rest_revalidated(&self, call: RestCall<'_>) -> Result<Revalidated> {
        let token = self.inner.tokens.get(call.host).await?;
        let mut hasher = DefaultHasher::new();
        token.hash(&mut hasher);
        // Keyed by credential, so one account never revalidates another's answer.
        let key = format!("{:x}\0{}\0{}", hasher.finish(), call.host, call.path);
        let previous = self.inner.etags.get(&key);
        let response = self
            .rest_with(call, previous.as_ref().map(|p| p.etag.clone()))
            .await?;
        if response.status == 304
            && let Some(previous) = previous
        {
            self.inner.etags.renew(&key);
            return Ok(Revalidated {
                headers: previous.headers,
                body: previous.body,
                changed: false,
            });
        }
        if !response.truncated {
            self.inner.etags.store(
                key,
                response.headers.get("etag"),
                &response.headers,
                &response.body,
            );
        }
        Ok(Revalidated {
            headers: response.headers,
            body: response.body,
            changed: true,
        })
    }

    async fn graphql_exchange(
        &self,
        host: &str,
        operation: &'static str,
        query: &str,
        variables: Value,
        allow_reserve: Option<bool>,
    ) -> Result<Envelope> {
        let body = json!({ "query": query, "variables": variables, "operationName": operation });
        let (response, envelope) = self
            .execute(Call {
                host,
                operation,
                api: Api::GraphQl,
                method: Method::Post,
                url: api_urls(host).graphql,
                headers: Headers::new().with("content-type", "application/json"),
                body: Some(serde_json::to_vec(&body).expect("JSON values serialize")),
                timeout: DEFAULT_TIMEOUT,
                max_body_bytes: DEFAULT_MAX_BODY_BYTES,
                allow_reserve,
            })
            .await?;
        match (response.status, envelope) {
            (_, Some(envelope)) => Ok(envelope),
            (404, None) => Err(Error::NotFound { operation }),
            (status, None) if response.truncated => Err(Error::Api {
                operation,
                status,
                messages: vec![format!(
                    "the answer is longer than {DEFAULT_MAX_BODY_BYTES} bytes"
                )],
            }),
            (status, None) => {
                let messages = match serde_json::from_slice::<Envelope>(&response.body) {
                    Ok(envelope) if !envelope.errors.is_empty() => {
                        envelope.errors.into_iter().map(|e| e.message).collect()
                    }
                    _ => rest_error_messages(&response.body),
                };
                Err(Error::Api {
                    operation,
                    status,
                    messages,
                })
            }
        }
    }

    /// Runs a GraphQL operation and returns its `data`. Any error fails the
    /// call: `NotFound` when every error is NOT_FOUND, else `GraphQl`.
    pub(crate) async fn graphql(
        &self,
        host: &str,
        operation: &'static str,
        variables: Value,
    ) -> Result<Value> {
        self.graphql_with(host, operation, variables, None).await
    }

    /// Like `graphql`, but may spend the quota reserve: small reads a write
    /// or an open picker depends on.
    pub(crate) async fn graphql_reserved(
        &self,
        host: &str,
        operation: &'static str,
        variables: Value,
    ) -> Result<Value> {
        self.graphql_with(host, operation, variables, Some(true))
            .await
    }

    async fn graphql_with(
        &self,
        host: &str,
        operation: &'static str,
        variables: Value,
        allow_reserve: Option<bool>,
    ) -> Result<Value> {
        let envelope = self
            .graphql_exchange(
                host,
                operation,
                graphql::operation(operation),
                variables,
                allow_reserve,
            )
            .await?;
        if !envelope.errors.is_empty() {
            return Err(graphql_failure(operation, envelope.errors));
        }
        envelope
            .data
            .ok_or_else(|| Error::decode(operation, "the answer has no data"))
    }

    /// Runs `operation` once per item in one aliased request. Each item
    /// fails on its own (a missing pull request is `NotFound`); errors that
    /// concern the whole request fail the call.
    pub(crate) async fn graphql_batch(
        &self,
        host: &str,
        operation: &'static str,
        items: &[Value],
    ) -> Result<Vec<Result<Value>>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let query = alias::aliased_operation(graphql::operation(operation), items.len());
        let envelope = self
            .graphql_exchange(
                host,
                operation,
                &query,
                alias::aliased_variables(items),
                None,
            )
            .await?;
        let (answers, general) = alias::split_answer(envelope.data, envelope.errors, items.len());
        if !general.is_empty() {
            return Err(graphql_failure(operation, general));
        }
        Ok(answers
            .into_iter()
            .map(|answer| {
                if answer.errors.is_empty() {
                    Ok(answer.data)
                } else {
                    Err(graphql_failure(operation, answer.errors))
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rest_error_messages_in_every_shape() {
        let body = br#"{"message":"Validation Failed","errors":["plain",{"message":"with message"},{"field":"head","code":"invalid"},{"resource":"Issue"}]}"#;
        assert_eq!(
            rest_error_messages(body),
            ["Validation Failed", "plain", "with message", "head invalid"]
        );
        assert!(rest_error_messages(b"<html>").is_empty());
    }
}
