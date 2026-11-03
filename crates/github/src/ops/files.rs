//! Which files the viewer has marked viewed, and marking them.

use super::pr_variables;
use crate::PullRequestRef;
use crate::client::Client;
use crate::convert::decode;
use crate::convert::files::files_viewed_page;
use crate::error::Result;
use crate::model::FilesViewed;
use serde_json::json;

/// Pages of a hundred files read at most.
const MAX_PAGES: usize = 5;
/// Files marked in one aliased request at most.
const FILES_PER_REQUEST: usize = 100;

impl Client {
    /// The viewed state of the pull request's files (up to 500).
    pub async fn files_viewed(&self, pr: &PullRequestRef) -> Result<FilesViewed> {
        let mut files = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut variables = pr_variables(pr)?;
            variables["after"] = json!(after);
            let data = self
                .graphql(&pr.repo.host, "PullRequestFilesViewed", variables)
                .await?;
            let (page, next) = files_viewed_page(decode("PullRequestFilesViewed", data)?);
            files.extend(page);
            match next {
                Some(next) if Some(&next) != after.as_ref() => after = Some(next),
                Some(_) => {
                    return Ok(FilesViewed {
                        files,
                        truncated: true,
                    });
                }
                None => {
                    return Ok(FilesViewed {
                        files,
                        truncated: false,
                    });
                }
            }
        }
        Ok(FilesViewed {
            files,
            truncated: true,
        })
    }

    /// Marks files viewed or not viewed. GitHub has no bulk mutation, so
    /// up to a hundred are aliased into one request; they run in order, so
    /// the last toggle of a path wins.
    pub async fn set_files_viewed(
        &self,
        pr: &PullRequestRef,
        files: &[(String, bool)],
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let pull_request_id = self.node_id(pr).await?;
        for chunk in files.chunks(FILES_PER_REQUEST) {
            let items: Vec<_> = chunk
                .iter()
                .map(|(path, viewed)| {
                    json!({ "pullRequestId": pull_request_id, "path": path, "viewed": viewed })
                })
                .collect();
            for answer in self
                .graphql_batch(&pr.repo.host, "SetFileViewed", &items)
                .await?
            {
                answer?;
            }
        }
        Ok(())
    }
}
