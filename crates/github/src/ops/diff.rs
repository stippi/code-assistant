//! Patches of a pull request or one of its commits, and file contents.

use super::{ensure_addressable, is_commit_sha, pr_variables};
use crate::client::{Client, RestCall};
use crate::convert::files::files_patch;
use crate::error::{Error, FileUnavailableReason, Result};
use crate::model::{DiffFileContents, DiffPage, FileChange};
use crate::{PullRequestRef, RepoRef};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

/// A large pull request's patch runs to megabytes; past this it is paged.
const DIFF_MAX_BYTES: usize = 8 * 1024 * 1024;
const DIFF_TIMEOUT: Duration = Duration::from_secs(60);
const FILE_MAX_BYTES: usize = 1024 * 1024;
const FILES_PAGE_SIZE: usize = 100;

/// A diff cursor is a page number, bounded so it stays a plain integer.
fn cursor_page(cursor: &str) -> Result<u32> {
    let valid = !cursor.is_empty()
        && cursor.len() <= 7
        && !cursor.starts_with('0')
        && cursor.bytes().all(|b| b.is_ascii_digit());
    valid
        .then(|| cursor.parse().ok())
        .flatten()
        .ok_or_else(|| Error::Invalid("The diff cursor was not one this client handed out.".into()))
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|part| urlencoding::encode(part).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

impl Client {
    /// The pull request's patch. Without a cursor the whole diff is asked
    /// for at once; past GitHub's limits (300 files, 8 MB) it continues as
    /// pages of the files API, and `next_cursor` names the next page.
    pub async fn diff(&self, pr: &PullRequestRef, cursor: Option<&str>) -> Result<DiffPage> {
        pr_variables(pr)?;
        let repo = &pr.repo;
        let files = format!(
            "repos/{}/{}/pulls/{}/files",
            repo.owner, repo.name, pr.number
        );
        if let Some(cursor) = cursor {
            return self
                .files_page(repo, &files, false, cursor_page(cursor)?)
                .await;
        }
        let whole = self
            .rest(
                RestCall::get(
                    &repo.host,
                    "diff",
                    format!("repos/{}/{}/pulls/{}", repo.owner, repo.name, pr.number),
                )
                .accept("application/vnd.github.diff")
                .max_body_bytes(DIFF_MAX_BYTES)
                .timeout(DIFF_TIMEOUT),
            )
            .await;
        match whole {
            Ok(response) if !response.truncated => Ok(DiffPage {
                patch: response.text(),
                truncated: false,
                next_cursor: None,
                omitted_file_stats: Vec::new(),
            }),
            Ok(_) => self.files_page(repo, &files, false, 1).await,
            // Above 300 changed files GitHub refuses the diff (406).
            Err(refused @ Error::Api { .. }) => self
                .files_page(repo, &files, false, 1)
                .await
                .map_err(|_| refused),
            Err(e) => Err(e),
        }
    }

    /// The patch of one commit, a page of files at a time.
    pub async fn commit_diff(
        &self,
        repo: &RepoRef,
        sha: &str,
        cursor: Option<&str>,
    ) -> Result<DiffPage> {
        ensure_addressable(repo)?;
        if !is_commit_sha(sha) {
            return Err(Error::Invalid(format!("'{sha}' is not a commit sha")));
        }
        let page = cursor.map(cursor_page).transpose()?.unwrap_or(1);
        let path = format!("repos/{}/{}/commits/{sha}", repo.owner, repo.name);
        self.files_page(repo, &path, true, page).await
    }

    async fn files_page(
        &self,
        repo: &RepoRef,
        path: &str,
        commit: bool,
        page: u32,
    ) -> Result<DiffPage> {
        let response = self
            .rest(
                RestCall::get(
                    &repo.host,
                    "diff",
                    format!("{path}?per_page={FILES_PAGE_SIZE}&page={page}"),
                )
                .max_body_bytes(DIFF_MAX_BYTES)
                .timeout(DIFF_TIMEOUT),
            )
            .await?;
        // A cut answer is an unparseable JSON prefix.
        if response.truncated {
            return Err(Error::decode(
                "diff",
                format!("page {page} of the changed files was too large to read"),
            ));
        }
        let entries: Vec<Value> = if commit {
            #[derive(Deserialize)]
            struct CommitFiles {
                #[serde(default)]
                files: Option<Vec<Value>>,
            }
            serde_json::from_slice::<CommitFiles>(&response.body)
                .map_err(|e| Error::decode("diff", e))?
                .files
                .unwrap_or_default()
        } else {
            serde_json::from_slice(&response.body).map_err(|e| Error::decode("diff", e))?
        };
        let files = files_patch(entries);
        // Raw counts, so malformed entries cannot end paging early.
        let more = files.raw_count >= FILES_PAGE_SIZE;
        Ok(DiffPage {
            patch: files.patch,
            truncated: files.truncated,
            next_cursor: more.then(|| (page + 1).to_string()),
            omitted_file_stats: files.omitted_file_stats,
        })
    }

    /// One file's text at a commit, up to 1 MB. Binary and larger files
    /// are refused with `FileUnavailable`.
    pub async fn file_contents(&self, repo: &RepoRef, path: &str, sha: &str) -> Result<String> {
        ensure_addressable(repo)?;
        if !is_commit_sha(sha) {
            return Err(Error::Invalid(format!("'{sha}' is not a commit sha")));
        }
        let response = self
            .rest(
                RestCall::get(
                    &repo.host,
                    "fileContents",
                    format!(
                        "repos/{}/{}/contents/{}?ref={}",
                        repo.owner,
                        repo.name,
                        encode_path(path),
                        urlencoding::encode(sha)
                    ),
                )
                .accept("application/vnd.github.raw+json")
                .max_body_bytes(FILE_MAX_BYTES)
                .timeout(DIFF_TIMEOUT),
            )
            .await?;
        let unavailable = |reason| Error::FileUnavailable {
            path: path.to_string(),
            reason,
        };
        if response.truncated {
            return Err(unavailable(FileUnavailableReason::Oversized));
        }
        match String::from_utf8(response.body) {
            Ok(text) if !text.contains('\0') => Ok(text),
            _ => Err(unavailable(FileUnavailableReason::Binary)),
        }
    }

    /// Both sides of one changed file of the pull request, or of one of its
    /// commits: the base (or the commit's parent) and the head (or commit).
    pub async fn diff_file_contents(
        &self,
        pr: &PullRequestRef,
        commit: Option<&str>,
        change: FileChange,
        old_path: &str,
        new_path: &str,
    ) -> Result<DiffFileContents> {
        pr_variables(pr)?;
        let repo = &pr.repo;
        if let Some(commit) = commit
            && !is_commit_sha(commit)
        {
            return Err(Error::Invalid(format!("'{commit}' is not a commit sha")));
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Sha {
            sha: Option<String>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Revisions {
            sha: Option<String>,
            parents: Vec<Sha>,
            base: Option<Sha>,
            head: Option<Sha>,
        }
        let path = match commit {
            Some(commit) => format!("repos/{}/{}/commits/{commit}", repo.owner, repo.name),
            None => format!("repos/{}/{}/pulls/{}", repo.owner, repo.name, pr.number),
        };
        let response = self
            .rest(RestCall::get(&repo.host, "diffFileContents", path).timeout(DIFF_TIMEOUT))
            .await?;
        let revisions: Revisions = serde_json::from_slice(&response.body).unwrap_or_default();
        let (base, head) = match commit {
            Some(_) => (
                revisions.parents.into_iter().next().and_then(|p| p.sha),
                revisions.sha,
            ),
            None => (
                revisions.base.and_then(|b| b.sha),
                revisions.head.and_then(|h| h.sha),
            ),
        };
        let head = head
            .filter(|sha| is_commit_sha(sha))
            .ok_or(Error::RevisionsUnavailable)?;
        // A root commit's new file has no parent to read.
        let base = base.filter(|sha| is_commit_sha(sha));
        if base.is_none() && !(commit.is_some() && change == FileChange::Added) {
            return Err(Error::RevisionsUnavailable);
        }
        let old = async {
            match (&base, change) {
                (Some(base), change) if change != FileChange::Added => {
                    self.file_contents(repo, old_path, base).await
                }
                _ => Ok(String::new()),
            }
        };
        let new = async {
            if change == FileChange::Deleted {
                Ok(String::new())
            } else {
                self.file_contents(repo, new_path, &head).await
            }
        };
        let (old, new) = futures::try_join!(old, new)?;
        Ok(DiffFileContents { old, new })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_cursors_it_hands_out() {
        assert_eq!(cursor_page("2").unwrap(), 2);
        assert_eq!(cursor_page("9999999").unwrap(), 9_999_999);
        for bad in ["", "0", "01", "1e3", "-1", "10000000", "abc"] {
            assert!(cursor_page(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn encodes_each_path_segment() {
        assert_eq!(encode_path("src/a b/c#d.rs"), "src/a%20b/c%23d.rs");
    }
}
