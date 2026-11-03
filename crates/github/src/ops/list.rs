//! Who is signed in, listings, line counts, batched summaries and head
//! branch lookups.

use super::{ensure_addressable, pr_variables};
use crate::client::{Client, RestCall};
use crate::convert::list::{RawHeadNode, RawSummary, head_pull_request, rows, summary};
use crate::convert::{Connection, decode};
use crate::error::{Error, Result};
use crate::model::{
    DiffStat, HeadPullRequest, Involvement, ListEntry, ListPage, ListQuery, ListState, Summary,
    Viewer,
};
use crate::{PullRequestRef, RepoRef};
use futures::future::join_all;
use serde::Deserialize;
use serde_json::{Value, json};

/// GitHub answers several aliased lookups faster in parallel batches of
/// this size than in one large one.
const ALIASES_PER_REQUEST: usize = 25;
/// `search` refuses `first` above this.
const MAX_ROWS: u32 = 100;
/// How far the unsearched fallback listing reads to fill a filtered page.
const FALLBACK_MAX_ROWS: usize = 1000;

/// Pull requests a head-branch lookup returns at most.
const HEAD_MATCHES: u32 = 20;

const SEARCH_CURSOR: &str = "s:";
const LIST_CURSOR: &str = "l:";

/// Quotes free text so GitHub reads it as one phrase, not as qualifiers.
fn search_phrase(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

pub(crate) fn search_query(repo: &RepoRef, query: &ListQuery) -> String {
    let mut terms = vec!["is:pr".to_string()];
    match query.state {
        ListState::All => {}
        ListState::Open => terms.push("is:open".into()),
        ListState::Closed => terms.extend(["is:closed".into(), "is:unmerged".into()]),
        ListState::Merged => terms.push("is:merged".into()),
    }
    match query.involvement {
        Involvement::All => {}
        Involvement::Authored => terms.push("author:@me".into()),
        Involvement::ReviewRequested => terms.push("review-requested:@me".into()),
    }
    if let Some(text) = query
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        terms.push(search_phrase(text));
    }
    terms.push("sort:updated-desc".into());
    terms.push(format!("repo:{}", repo.full_name()));
    terms.join(" ")
}

fn involves(entry: &ListEntry, involvement: Involvement, viewer: &str) -> bool {
    match involvement {
        Involvement::All => true,
        Involvement::Authored => entry
            .author
            .as_ref()
            .is_some_and(|a| a.login.eq_ignore_ascii_case(viewer)),
        Involvement::ReviewRequested => {
            entry.has_team_review_request
                || entry
                    .review_request_logins
                    .iter()
                    .any(|login| login.eq_ignore_ascii_case(viewer))
        }
    }
}

/// Groups references by host, keeping each one's position.
fn by_host(prs: &[PullRequestRef]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, pr) in prs.iter().enumerate() {
        match groups.iter_mut().find(|(host, _)| *host == pr.repo.host) {
            Some((_, indexes)) => indexes.push(index),
            None => groups.push((pr.repo.host.clone(), vec![index])),
        }
    }
    groups
}

impl Client {
    /// The account the host's credential signs in as.
    pub async fn viewer(&self, host: &str) -> Result<Viewer> {
        #[derive(Deserialize)]
        struct User {
            id: u64,
            login: String,
        }
        match self
            .rest(RestCall::get(host, "viewer", "user".into()).allow_reserve())
            .await
        {
            Ok(response) => {
                let user: User = serde_json::from_slice(&response.body)
                    .map_err(|e| Error::decode("viewer", e))?;
                Ok(Viewer {
                    id: user.id,
                    login: user.login,
                })
            }
            // A token scoped to repositories may not read `user`; GraphQL still says.
            Err(Error::Api { status: 403, .. }) => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct GraphQlViewer {
                    database_id: u64,
                    login: String,
                }
                #[derive(Deserialize)]
                struct Data {
                    viewer: GraphQlViewer,
                }
                let data = self
                    .graphql_reserved(host, "RoutingIdentityViewer", json!({}))
                    .await?;
                let data: Data = decode("RoutingIdentityViewer", data)?;
                Ok(Viewer {
                    id: data.viewer.database_id,
                    login: data.viewer.login,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// A page of the repository's pull requests, newest update first. Uses
    /// search; a repository search does not cover (it answers with silence)
    /// is listed without it, newest created first.
    pub async fn list(&self, repo: &RepoRef, query: &ListQuery) -> Result<ListPage> {
        ensure_addressable(repo)?;
        let limit = query.limit.clamp(1, MAX_ROWS);
        let mut page = match query.cursor.as_deref() {
            Some(cursor) => {
                if let Some(after) = cursor.strip_prefix(SEARCH_CURSOR) {
                    self.search_page(repo, query, limit, Some(after)).await?
                } else if let Some(after) = cursor.strip_prefix(LIST_CURSOR) {
                    self.fallback_page(repo, query, limit, Some(after)).await?
                } else {
                    return Err(Error::Invalid(
                        "The list cursor was not one this client handed out.".into(),
                    ));
                }
            }
            None => {
                let page = self.search_page(repo, query, limit, None).await?;
                let has_text = query.text.as_deref().is_some_and(|t| !t.trim().is_empty());
                if page.entries.is_empty() && !has_text {
                    self.fallback_page(repo, query, limit, None).await?
                } else {
                    page
                }
            }
        };
        self.mark_stacks(repo, &mut page.entries).await;
        Ok(page)
    }

    async fn search_page(
        &self,
        repo: &RepoRef,
        query: &ListQuery,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ListPage> {
        let variables = json!({
            "q": search_query(repo, query),
            "first": limit,
            "after": after,
            "stats": true,
        });
        let data = self
            .graphql(&repo.host, "PullRequestSearch", variables)
            .await?;
        let page = rows(data.get("search").cloned());
        Ok(ListPage {
            entries: page.entries,
            next_cursor: page.next_cursor.map(|c| format!("{SEARCH_CURSOR}{c}")),
        })
    }

    async fn fallback_page(
        &self,
        repo: &RepoRef,
        query: &ListQuery,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ListPage> {
        let viewer = match query.involvement {
            Involvement::All => None,
            _ => Some(self.viewer(&repo.host).await?.login),
        };
        let states = match query.state {
            ListState::All => Value::Null,
            ListState::Open => json!(["OPEN"]),
            ListState::Closed => json!(["CLOSED"]),
            ListState::Merged => json!(["MERGED"]),
        };
        let limit = limit as usize;
        let mut entries = Vec::new();
        let mut scanned = 0;
        let mut after = after.map(str::to_string);
        // Each page asks only for the rows still missing, so none is skipped.
        let next_cursor = loop {
            let variables = json!({
                "owner": repo.owner,
                "name": repo.name,
                "states": states,
                "first": limit - entries.len(),
                "after": after,
                "stats": true,
            });
            let data = self
                .graphql(&repo.host, "PullRequestList", variables)
                .await?;
            let page = rows(data.pointer("/repository/pullRequests").cloned());
            scanned += page.raw_count;
            entries.extend(page.entries.into_iter().filter(|entry| {
                viewer
                    .as_deref()
                    .is_none_or(|viewer| involves(entry, query.involvement, viewer))
            }));
            let next = page.next_cursor;
            if next.is_none() || next == after {
                break None;
            }
            if entries.len() >= limit || scanned >= FALLBACK_MAX_ROWS {
                break next;
            }
            after = next;
        };
        Ok(ListPage {
            entries,
            next_cursor: next_cursor.map(|c| format!("{LIST_CURSOR}{c}")),
        })
    }

    /// Line counts of several pull requests, aliased 25 to a request. A pull
    /// request GitHub cannot find is left out.
    pub async fn list_stats(&self, prs: &[PullRequestRef]) -> Result<Vec<DiffStat>> {
        #[derive(Deserialize)]
        struct Stats {
            additions: u64,
            deletions: u64,
        }
        let answers = self.batched(prs, "PullRequestStats").await?;
        let mut stats = Vec::new();
        for (pr, answer) in prs.iter().zip(answers) {
            let data = match answer {
                Ok(data) => data,
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e),
            };
            if let Some(Ok(read)) = data
                .pointer("/repository/pullRequest")
                .filter(|v| !v.is_null())
                .map(|v| serde_json::from_value::<Stats>(v.clone()))
            {
                stats.push(DiffStat {
                    pr: pr.clone(),
                    additions: read.additions,
                    deletions: read.deletions,
                });
            }
        }
        Ok(stats)
    }

    /// The summary of each pull request, in order, aliased 25 to a request.
    /// Each one fails on its own; a batch that fails fails all of its items.
    pub async fn summaries(&self, prs: &[PullRequestRef]) -> Vec<Result<Summary>> {
        let answers = match self.batched(prs, "PullRequestSummary").await {
            Ok(answers) => answers,
            Err(e) => return prs.iter().map(|_| Err(e.clone())).collect(),
        };
        answers
            .into_iter()
            .map(|answer| {
                let data = answer?;
                let raw = data
                    .pointer("/repository/pullRequest")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .ok_or(Error::NotFound {
                        operation: "PullRequestSummary",
                    })?;
                let raw: RawSummary = decode("PullRequestSummary", raw)?;
                Ok(summary(raw))
            })
            .collect()
    }

    /// Runs an aliased per-pull-request operation for all `prs`, batched per
    /// host, and returns one answer per reference, in order. A reference
    /// GitHub cannot address fails on its own without a request.
    pub(crate) async fn batched(
        &self,
        prs: &[PullRequestRef],
        operation: &'static str,
    ) -> Result<Vec<Result<Value>>> {
        let mut answers: Vec<Option<Result<Value>>> = prs.iter().map(|_| None).collect();
        let mut requests = Vec::new();
        for (host, indexes) in by_host(prs) {
            let mut valid = Vec::new();
            for index in indexes {
                match pr_variables(&prs[index]) {
                    Ok(variables) => valid.push((index, variables)),
                    Err(e) => answers[index] = Some(Err(e)),
                }
            }
            for chunk in valid.chunks(ALIASES_PER_REQUEST) {
                let host = host.clone();
                let indexes: Vec<usize> = chunk.iter().map(|(i, _)| *i).collect();
                let items: Vec<Value> = chunk.iter().map(|(_, v)| v.clone()).collect();
                requests.push(async move {
                    let answers = self.graphql_batch(&host, operation, &items).await;
                    (indexes, answers)
                });
            }
        }
        for (indexes, batch) in join_all(requests).await {
            let batch = batch?;
            for (index, answer) in indexes.into_iter().zip(batch) {
                answers[index] = Some(answer);
            }
        }
        Ok(answers
            .into_iter()
            .map(|a| a.expect("every item answered"))
            .collect())
    }

    /// The pull requests whose head is `branch`, newest first (any state).
    pub async fn pull_requests_for_head(
        &self,
        repo: &RepoRef,
        branch: &str,
    ) -> Result<Vec<HeadPullRequest>> {
        ensure_addressable(repo)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Repository {
            #[serde(default)]
            pull_requests: Connection<Value>,
        }
        #[derive(Deserialize)]
        struct Data {
            repository: Option<Repository>,
        }
        let variables = json!({
            "owner": repo.owner,
            "name": repo.name,
            "head": branch,
            "states": Value::Null,
            "first": HEAD_MATCHES,
        });
        let data = self
            .graphql(&repo.host, "PullRequestsByHead", variables)
            .await?;
        let data: Data = decode("PullRequestsByHead", data)?;
        let repository = data.repository.ok_or(Error::NotFound {
            operation: "PullRequestsByHead",
        })?;
        Ok(repository
            .pull_requests
            .into_nodes()
            .filter_map(|node| serde_json::from_value::<RawHeadNode>(node).ok())
            .map(head_pull_request)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_search_that_free_text_cannot_widen() {
        let repo = RepoRef::new("github.com", "stippi", "code-assistant");
        let query = ListQuery {
            state: ListState::Closed,
            involvement: Involvement::ReviewRequested,
            text: Some(r#"fix" repo:other/repo \"#.into()),
            limit: 30,
            cursor: None,
        };
        assert_eq!(
            search_query(&repo, &query),
            r#"is:pr is:closed is:unmerged review-requested:@me "fix\" repo:other/repo \\" sort:updated-desc repo:stippi/code-assistant"#
        );
        let open = ListQuery {
            limit: 30,
            ..ListQuery::default()
        };
        assert_eq!(
            search_query(&repo, &open),
            "is:pr is:open sort:updated-desc repo:stippi/code-assistant"
        );
    }
}
