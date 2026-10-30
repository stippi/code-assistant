//! Naming a repository and a pull request on a GitHub host.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A repository on a GitHub host (github.com or an Enterprise server).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoRef {
    /// Lower-case host name, e.g. `github.com`.
    pub host: String,
    pub owner: String,
    pub name: String,
}

/// One pull request of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub repo: RepoRef,
    pub number: u64,
}

impl RepoRef {
    pub fn new(host: impl Into<String>, owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            host: normalize_host(&host.into()),
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// Reads a git remote URL: `https://host/owner/name(.git)`,
    /// `git@host:owner/name(.git)` and `ssh://git@host(:port)/owner/name(.git)`.
    /// Returns `None` for anything that does not name exactly one repository
    /// on a plausible host. A port on an ssh URL is the ssh port, not the web
    /// host's, so it is dropped.
    pub fn from_remote_url(url: &str) -> Option<Self> {
        let url = url.trim();
        let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
            let (authority, path) = rest.split_once('/')?;
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            let host = match scheme {
                "ssh" | "git" | "git+ssh" | "ssh+git" => host.split(':').next()?,
                "https" | "http" => host,
                _ => return None,
            };
            (host, path)
        } else {
            // scp-like syntax: [user@]host:owner/name
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            (host, path)
        };
        let host = normalize_host(host);
        if !is_github_host(&host) {
            return None;
        }
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path.split_once('/')?;
        if !is_repository_part(owner) || !is_repository_part(name) {
            return None;
        }
        Some(Self::new(host, owner, name))
    }

    /// `owner/name`.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// The pull request `number` of this repository.
    pub fn pull_request(&self, number: u64) -> PullRequestRef {
        PullRequestRef {
            repo: self.clone(),
            number,
        }
    }

    /// Whether the owner and name are safe to put into a request path or a
    /// search qualifier.
    pub(crate) fn is_addressable(&self) -> bool {
        is_github_host(&self.host)
            && is_repository_part(&self.owner)
            && is_repository_part(&self.name)
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.host, self.owner, self.name)
    }
}

impl fmt::Display for PullRequestRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

pub(crate) fn normalize_host(host: &str) -> String {
    host.trim().to_ascii_lowercase()
}

/// API URLs are built from the host text, so anything but a bare host name
/// (with an optional port) could send a token elsewhere.
pub(crate) fn is_github_host(host: &str) -> bool {
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    let port_ok = port.is_none_or(|port| {
        !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
    });
    port_ok
        && !name.is_empty()
        && name.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

fn is_repository_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// github.com and GHE.com tenancies share one API layout, Enterprise
/// servers serve the API below `/api`.
pub(crate) struct ApiUrls {
    pub rest: String,
    pub graphql: String,
}

pub(crate) fn api_urls(host: &str) -> ApiUrls {
    if host == "github.com" {
        ApiUrls {
            rest: "https://api.github.com".into(),
            graphql: "https://api.github.com/graphql".into(),
        }
    } else if host.ends_with(".ghe.com") {
        ApiUrls {
            rest: format!("https://api.{host}"),
            graphql: format!("https://api.{host}/graphql"),
        }
    } else {
        ApiUrls {
            rest: format!("https://{host}/api/v3"),
            graphql: format!("https://{host}/api/graphql"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(url: &str) -> Option<(String, String, String)> {
        RepoRef::from_remote_url(url).map(|r| (r.host, r.owner, r.name))
    }

    fn repo(host: &str, owner: &str, name: &str) -> Option<(String, String, String)> {
        Some((host.into(), owner.into(), name.into()))
    }

    #[test]
    fn reads_https_remotes_with_and_without_git_suffix() {
        let expected = repo("github.com", "stippi", "code-assistant");
        assert_eq!(
            parsed("https://github.com/stippi/code-assistant.git"),
            expected
        );
        assert_eq!(parsed("https://github.com/stippi/code-assistant"), expected);
        assert_eq!(
            parsed("https://github.com/stippi/code-assistant/"),
            expected
        );
        assert_eq!(
            parsed("https://user@GitHub.com/stippi/code-assistant"),
            expected
        );
    }

    #[test]
    fn reads_scp_style_ssh_remotes() {
        assert_eq!(
            parsed("git@github.com:stippi/code-assistant.git"),
            repo("github.com", "stippi", "code-assistant")
        );
        assert_eq!(
            parsed("github.example.com:team/tool"),
            repo("github.example.com", "team", "tool")
        );
    }

    #[test]
    fn reads_ssh_urls_and_drops_the_ssh_port() {
        assert_eq!(
            parsed("ssh://git@github.com/stippi/code-assistant.git"),
            repo("github.com", "stippi", "code-assistant")
        );
        assert_eq!(
            parsed("ssh://git@ghe.corp:2222/team/tool"),
            repo("ghe.corp", "team", "tool")
        );
    }

    #[test]
    fn keeps_the_port_of_an_https_enterprise_host() {
        assert_eq!(
            parsed("https://ghe.corp:8443/team/tool.git"),
            repo("ghe.corp:8443", "team", "tool")
        );
    }

    #[test]
    fn refuses_what_does_not_name_one_repository() {
        assert_eq!(parsed("https://github.com/stippi"), None);
        assert_eq!(parsed("https://github.com/a/b/c"), None);
        assert_eq!(parsed("/local/path/repo.git"), None);
        assert_eq!(parsed("file:///tmp/repo.git"), None);
        assert_eq!(parsed("https://evil.com@/a/b"), None);
        assert_eq!(parsed("https://github.com/../b"), None);
    }

    #[test]
    fn api_urls_follow_the_host_kind() {
        assert_eq!(api_urls("github.com").rest, "https://api.github.com");
        assert_eq!(
            api_urls("acme.ghe.com").graphql,
            "https://api.acme.ghe.com/graphql"
        );
        assert_eq!(api_urls("ghe.corp").rest, "https://ghe.corp/api/v3");
        assert_eq!(api_urls("ghe.corp").graphql, "https://ghe.corp/api/graphql");
    }
}
