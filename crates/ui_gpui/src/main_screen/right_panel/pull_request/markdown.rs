//! Pull request text (descriptions, comments) prepared for the Markdown
//! view, which knows CommonMark but not GitHub's additions: an alert
//! (`> [!NOTE]`) becomes a quote headed by its kind, and the references
//! GitHub links by itself (`#12`, `repo#12`, `owner/repo#12`, a commit SHA,
//! `repo@sha`, `@user`, `@org/team`) become links. Code (fenced and inline),
//! links, HTML tags and URLs stay as written.
//!
//! The alert keeps the quote's look: the Markdown view has no coloured
//! callouts, so the kind shows as an emoji and a bold title.

use github::RepoRef;

/// Hex digits of a SHA a link shows.
const SHORT_SHA: usize = 7;

/// `text` with alerts and references rewritten for the Markdown view.
pub fn prepare(text: &str, repo: &RepoRef) -> String {
    let links = Links::new(repo);
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut fence: Option<Fence> = None;
    let mut in_quote = false;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let eol = &line[body.len()..];
        if let Some(open) = &fence {
            if open.is_closed_by(body) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if let Some(opened) = Fence::opened_by(body) {
            fence = Some(opened);
            in_quote = false;
            out.push_str(line);
            continue;
        }
        let quote_line = body.trim_start().starts_with('>');
        if !in_quote && let Some((prefix, title)) = alert_title(body) {
            let bare = prefix.trim_end();
            let eol = if eol.is_empty() { "\n" } else { eol };
            out.push_str(&format!("{prefix}**{title}**{eol}{bare}{eol}"));
            in_quote = true;
            continue;
        }
        in_quote = quote_line;
        links.rewrite_line(body, &mut out);
        out.push_str(eol);
    }
    out
}

/// An open code fence: its character and length.
struct Fence {
    marker: char,
    len: usize,
}

impl Fence {
    fn opened_by(line: &str) -> Option<Fence> {
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() > 3 {
            return None;
        }
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let len = trimmed.chars().take_while(|&c| c == marker).count();
        let info = &trimmed[len..];
        (len >= 3 && !(marker == '`' && info.contains('`'))).then_some(Fence { marker, len })
    }

    fn is_closed_by(&self, line: &str) -> bool {
        let trimmed = line.trim_start_matches(' ');
        let len = trimmed.chars().take_while(|&c| c == self.marker).count();
        line.len() - trimmed.len() <= 3 && len >= self.len && trimmed[len..].trim().is_empty()
    }
}

/// For the first line of an alert, its quote prefix (`> `) and the title
/// replacing `[!KIND]`.
fn alert_title(line: &str) -> Option<(&str, &'static str)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let after_quote = trimmed.strip_prefix('>')?;
    let marker = after_quote.strip_prefix(' ').unwrap_or(after_quote);
    let kind = marker.strip_prefix("[!")?.split_once(']')?;
    if !kind.1.trim().is_empty() {
        return None;
    }
    let title = match kind.0.to_ascii_uppercase().as_str() {
        "NOTE" => "ℹ️ Note",
        "TIP" => "💡 Tip",
        "IMPORTANT" => "❗ Important",
        "WARNING" => "⚠️ Warning",
        "CAUTION" => "🛑 Caution",
        _ => return None,
    };
    Some((&line[..line.len() - marker.len()], title))
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A reference may not follow these: they would make it part of a word, a
/// path, an escape or a name.
fn is_boundary(previous: Option<char>) -> bool {
    previous.is_none_or(|c| !(is_word(c) || matches!(c, '.' | '/' | '\\' | '-')))
}

/// The length of the leading run of `s` whose characters pass `keep`.
fn run(s: &str, keep: impl Fn(char) -> bool) -> usize {
    s.find(|c| !keep(c)).unwrap_or(s.len())
}

fn not_followed_by_word(rest: &str) -> bool {
    !rest.chars().next().is_some_and(is_word)
}

/// The links of one repository.
struct Links<'a> {
    origin: String,
    repo: &'a RepoRef,
}

impl<'a> Links<'a> {
    fn new(repo: &'a RepoRef) -> Self {
        Self {
            origin: format!("https://{}", repo.host),
            repo,
        }
    }

    /// `owner/name` of a reference's qualifier: `owner/repo` as given, a
    /// bare `repo` of the same owner, none this repository.
    fn repository(&self, qualifier: Option<&str>) -> String {
        match qualifier {
            Some(q) if q.contains('/') => q.to_owned(),
            Some(q) => format!("{q}/{}", self.repo.name),
            None => self.repo.full_name(),
        }
    }

    /// Copies `line` to `out` with its references linked.
    fn rewrite_line(&self, line: &str, out: &mut String) {
        let mut i = 0;
        while i < line.len() {
            let rest = &line[i..];
            let verbatim = verbatim_len(line, i);
            if verbatim > 0 {
                out.push_str(&rest[..verbatim]);
                i += verbatim;
                continue;
            }
            if is_boundary(line[..i].chars().next_back())
                && let Some((len, label, url)) = self.reference(rest)
            {
                let label = label.replace('_', "\\_");
                out.push_str(&format!("[{label}]({url})"));
                i += len;
                continue;
            }
            let c = rest.chars().next().expect("i is inside the line");
            out.push(c);
            i += c.len_utf8();
        }
    }

    /// A reference at the start of `s`: its length, label and URL.
    fn reference(&self, s: &str) -> Option<(usize, String, String)> {
        if let Some(rest) = s.strip_prefix('@') {
            return self.mention(rest);
        }
        if let Some(rest) = s.strip_prefix('#') {
            let len = issue_number_len(rest)?;
            let url = format!(
                "{}/{}/issues/{}",
                self.origin,
                self.repository(None),
                &rest[..len]
            );
            return Some((len + 1, s[..len + 1].to_owned(), url));
        }
        self.qualified(s).or_else(|| self.commit(s, None, 0))
    }

    fn mention(&self, rest: &str) -> Option<(usize, String, String)> {
        let login =
            rest[..run(rest, |c| c.is_ascii_alphanumeric() || c == '-')].trim_end_matches('-');
        if !login.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return None;
        }
        let after = &rest[login.len()..];
        if let Some(team_rest) = after.strip_prefix('/')
            && team_rest.starts_with(|c: char| c.is_ascii_alphanumeric())
        {
            let team = &team_rest[..run(team_rest, |c| c.is_ascii_alphanumeric() || c == '-')];
            if not_followed_by_word(&team_rest[team.len()..]) {
                let len = 1 + login.len() + 1 + team.len();
                let url = format!("{}/orgs/{login}/teams/{team}", self.origin);
                return Some((len, format!("@{login}/{team}"), url));
            }
        }
        not_followed_by_word(after).then(|| {
            (
                1 + login.len(),
                format!("@{login}"),
                format!("{}/{login}", self.origin),
            )
        })
    }

    /// `repo#12`, `owner/repo#12`, `repo@sha` or `owner/repo@sha`.
    fn qualified(&self, s: &str) -> Option<(usize, String, String)> {
        let part = |s: &str| run(s, |c| is_word(c) || c == '.' || c == '-');
        let owner = part(s);
        let mut len = owner;
        if let Some(name) = s[len..].strip_prefix('/').map(part).filter(|&n| n > 0) {
            len += 1 + name;
        } else if s[..owner].contains('.') {
            return None;
        }
        if owner == 0 {
            return None;
        }
        let qualifier = &s[..len];
        let rest = &s[len..];
        if let Some(number) = rest.strip_prefix('#') {
            let digits = issue_number_len(number)?;
            let end = len + 1 + digits;
            let url = format!(
                "{}/{}/issues/{}",
                self.origin,
                self.repository(Some(qualifier)),
                &number[..digits]
            );
            return Some((end, s[..end].to_owned(), url));
        }
        let sha = rest.strip_prefix('@')?;
        self.commit(sha, Some(qualifier), len + 1)
    }

    /// A SHA at the start of `s`, `offset` bytes into the reference.
    fn commit(
        &self,
        s: &str,
        qualifier: Option<&str>,
        offset: usize,
    ) -> Option<(usize, String, String)> {
        let len = run(s, |c| c.is_ascii_hexdigit());
        let sha = &s[..len];
        // A run of digits is a number far more often than a SHA.
        let is_sha = (7..=40).contains(&len)
            && not_followed_by_word(&s[len..])
            && sha.bytes().any(|b| b.is_ascii_alphabetic());
        is_sha.then(|| {
            let label = match qualifier {
                Some(q) => format!("{q}@{}", &sha[..SHORT_SHA]),
                None => sha[..SHORT_SHA].to_owned(),
            };
            let url = format!(
                "{}/{}/commit/{sha}",
                self.origin,
                self.repository(qualifier)
            );
            (offset + len, label, url)
        })
    }
}

/// `[1-9][0-9]*` not followed by a word character.
fn issue_number_len(s: &str) -> Option<usize> {
    let len = run(s, |c| c.is_ascii_digit());
    (len > 0 && !s.starts_with('0') && not_followed_by_word(&s[len..])).then_some(len)
}

/// How much of `line` from `i` must be copied untouched: a code span, an
/// HTML tag or autolink, a Markdown link or image, or a bare URL.
fn verbatim_len(line: &str, i: usize) -> usize {
    let rest = &line[i..];
    match rest.as_bytes()[0] {
        b'`' => {
            let ticks = run(rest, |c| c == '`');
            let closing = "`".repeat(ticks);
            let mut search = ticks;
            while let Some(at) = rest[search..].find(&closing) {
                let end = search + at + ticks;
                // A longer run does not close it.
                if !rest[end..].starts_with('`') {
                    return end;
                }
                search = end + run(&rest[end..], |c| c == '`');
            }
            ticks
        }
        b'<' if rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || "/!?".contains(c)) => {
            rest.find('>').map_or(0, |end| end + 1)
        }
        b'[' | b'!' => link_len(rest),
        b'h' | b'w' if is_boundary(line[..i].chars().next_back()) => {
            let is_url = ["https://", "http://", "www."]
                .iter()
                .any(|scheme| rest.starts_with(scheme));
            if is_url {
                run(rest, |c| !c.is_whitespace() && c != '<')
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// `[text](target)`, `![alt](src)` or `[text][ref]` at the start of `s`.
fn link_len(s: &str) -> usize {
    let start = usize::from(s.starts_with('!'));
    if !s[start..].starts_with('[') {
        return 0;
    }
    let Some(text_end) = closing(&s[start..], '[', ']').map(|end| start + end) else {
        return 0;
    };
    let after = &s[text_end + 1..];
    let target = match after.chars().next() {
        Some('(') => closing(after, '(', ')'),
        Some('[') => closing(after, '[', ']'),
        _ => None,
    };
    target.map_or(0, |end| text_end + 1 + end + 1)
}

/// The index of the bracket closing the one `s` starts with.
fn closing(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::repo;

    fn prepared(text: &str) -> String {
        prepare(text, &repo())
    }

    const ISSUES: &str = "https://github.com/stippi/code-assistant/issues";

    #[test]
    fn alerts_become_titled_quotes() {
        assert_eq!(
            prepared("> [!NOTE]\n> Images show as images.\n"),
            "> **ℹ️ Note**\n>\n> Images show as images.\n"
        );
        assert_eq!(
            prepared(">[!warning]  \n>Careful"),
            ">**⚠️ Warning**\n>\n>Careful"
        );
    }

    #[test]
    fn only_a_quotes_first_line_is_an_alert() {
        let text = "> Intro\n> [!TIP]\n\n[!NOTE] plain\n> [!NOTE] trailing text\n> [!UNKNOWN]";
        assert_eq!(prepared(text), text);
    }

    #[test]
    fn issue_references_link() {
        assert_eq!(
            prepared("Fixes #12, see foo/bar#3 and other#4."),
            format!(
                "Fixes [#12]({ISSUES}/12), see [foo/bar#3](https://github.com/foo/bar/issues/3) \
                 and [other#4](https://github.com/other/code-assistant/issues/4)."
            )
        );
    }

    #[test]
    fn commits_link_shortened() {
        assert_eq!(
            prepared("In d9aea40f2b and foo/bar@abcdef12."),
            "In [d9aea40](https://github.com/stippi/code-assistant/commit/d9aea40f2b) and \
             [foo/bar@abcdef1](https://github.com/foo/bar/commit/abcdef12)."
        );
    }

    #[test]
    fn mentions_link_to_profiles_and_teams() {
        assert_eq!(
            prepared("cc @octo-cat and @acme/core-team"),
            "cc [@octo-cat](https://github.com/octo-cat) and \
             [@acme/core-team](https://github.com/orgs/acme/teams/core-team)"
        );
    }

    #[test]
    fn lookalikes_stay_text() {
        let text = "mail me@example.com, #0, #12abc, x/y/#3, 1234567, deadbeefz, \\#5, a.b#3";
        assert_eq!(prepared(text), text);
    }

    #[test]
    fn code_links_tags_and_urls_stay_untouched() {
        let text = "`#12` and ``a `#1` b`` [#12](https://x) ![#3](i.png) [#4][r] \
                    <a href=\"#5\"> https://github.com/o/r/pull/1#6 www.x.io/@me";
        assert_eq!(prepared(text), text);
    }

    #[test]
    fn fenced_code_stays_untouched() {
        // A shorter fence does not close a longer one.
        let text = "````rust\n// #12 @me\n```\nstill code #1\n````\nafter #2\n~~~\n#3\n~~~";
        assert_eq!(
            prepared(text),
            format!(
                "````rust\n// #12 @me\n```\nstill code #1\n````\nafter [#2]({ISSUES}/2)\n~~~\n#3\n~~~"
            )
        );
    }

    #[test]
    fn brackets_without_a_target_are_text() {
        assert_eq!(
            prepared("[WIP] fixes #7"),
            format!("[WIP] fixes [#7]({ISSUES}/7)")
        );
    }

    #[test]
    fn underscores_in_labels_are_escaped() {
        assert_eq!(
            prepared("my_org/my_repo#9"),
            "[my\\_org/my\\_repo#9](https://github.com/my_org/my_repo/issues/9)"
        );
    }
}
