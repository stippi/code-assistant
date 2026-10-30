# Pull request view

A GitHub pull request page in the right panel: the conversation, the
timeline and the changed files with inline review threads, plus everything
needed to act on a PR (comment, review, reply, resolve, react, label,
request reviewers, mark files viewed, merge). Modelled closely on T3 Code's
pull request panel (`~/workspace/t3code`, `apps/web/src/components/pullRequest/`,
`packages/source-control-github/src/server/`, `apps/server/src/pullRequest/`),
including how it stays fast.

## Layers

```
github (Layer 0)            HTTP client for api.github.com: auth via gh,
                            REST + GraphQL, quota, ETags, typed model,
                            JSON → model mapping, every PR operation
git (Layer 0)               + patch.rs: unified diff text → files/hunks
code_assistant_core         pull_requests/: repo resolution for a session,
  (Layer 3)                 read cache (TTL, coalescing, stale-while-
                            revalidate, disk snapshots), invalidation after
                            writes, app-scoped UiEvent; SessionService methods
ui_gpui (Layer 4)           diff/: shared diff model + row rendering (Review
                            and PR Files tab), right_panel/pull_request/: the
                            panel, tabs, composer, pickers
```

### `github` crate

- **Auth**: `GH_TOKEN` / `GITHUB_TOKEN`, else `gh auth token --hostname <host>`
  (10 s timeout). Tokens are cached 5 min. A 401 drops the cached token and
  retries once. Only `gh` supplies the token; requests go over HTTP.
- **Transport seam**: `trait Transport { async fn send(Request) -> Response }`
  with a reqwest implementation and a scripted fake (feature `test-utils`)
  that answers by GraphQL operation name or REST method + path. Every
  higher-level test (crate and core) runs against the fake. There is no
  second trait over the whole API.
- **HTTP**: `Authorization: Bearer`, `x-github-api-version: 2022-11-28`,
  `user-agent: code-assistant`. github.com → `https://api.github.com` and
  `/graphql`; GHE → `https://<host>/api/v3` and `/api/graphql`. At most 8
  concurrent requests (secondary limits), 30 s timeout (60 s for diffs),
  8 MB body cap.
- **Quota**: record `x-ratelimit-*` per host and resource. Background reads
  stop below 10 % remaining; interactive reads may use quota down to 1.
  429 or a rate-limited 403 pauses the host until `retry-after` /
  `x-ratelimit-reset` (else 30 s, doubling to 15 min). GraphQL 200s carrying
  `RATE_LIMITED` count as rate limited too.
- **ETag cache** for REST GETs (512 entries, 1 h): `If-None-Match`, a 304
  returns the stored body (costs no quota). Used by the checks revalidator.
- **GraphQL documents** ported from T3's `.graphql` files (`include_str!`):
  `PullRequestCore`, `PullRequestCheckContexts`, `PullRequestActivity`,
  `PullRequestCommitStats`, `ReviewThreads`, `ReviewThreadComments`,
  `PullRequestSearch`, `PullRequestList`, `PullRequestSummary` (aliased,
  25 per request), `PullRequestFilesViewed`, `ReviewerCandidates`,
  `LabelCandidates`, `PullRequestActionState` and the mutations
  (`AddComment`, `UpdateIssueComment`, `UpdateReviewComment`,
  `UpdatePullRequest`, `AddReviewThreadReply`, `Resolve/UnresolveReviewThread`,
  `AddReaction`/`RemoveReaction`, `SetFileViewed` aliased up to 100,
  `MergePullRequest`, `EnablePullRequestAutoMerge`,
  `DisablePullRequestAutoMerge`, `UpdatePullRequestBranch`,
  `MarkPullRequestReadyForReview`, `ConvertPullRequestToDraft`,
  `ClosePullRequest`, `ReopenPullRequest`). Node ids come from
  `PullRequestNodeId` (LRU 128).
- **REST**: `GET user` (viewer), diff `GET pulls/N` with
  `Accept: application/vnd.github.diff` (on truncation or 406 fall back to
  `pulls/N/files?per_page=100&page=k`, rebuilt as patch text, cursor = page),
  commit diff `commits/SHA`, file contents `contents/<path>?ref=SHA`
  (`application/vnd.github.raw+json`, 1 MB cap), checks revalidation
  (`pulls/N`, `commits/SHA/check-runs?filter=all`, `commits/SHA/status`),
  submit review `POST pulls/N/reviews`, reviewers
  `POST/DELETE pulls/N/requested_reviewers`, labels `POST issues/N/labels`,
  `DELETE issues/N/labels/{name}`.
- **Model** (serde, also persisted): `RepoRef {host, owner, name}`
  (+ `from_remote_url`), `PullRequestRef {repo, number}`, `Actor`, `Label`,
  `Check`, `ChecksState`, `ListEntry`, `Summary`, `Detail` (body, head sha,
  reviewers, labels, checks, merge capabilities, viewer permissions,
  behind-by, auto-merge, maintainer-can-modify), `Activity` (comments of
  kind issue-comment / review-comment / review, review threads, commits,
  reactions), `ReviewThread`, `ThreadComment`, `Reaction`, `Commit`,
  `DiffPage {patch, truncated, next_cursor, omitted_file_stats}`,
  `FileViewed {path, state: Unviewed|Viewed|Dismissed}`. Field lists follow
  `t3code/packages/contracts/src/pullRequest.ts`.

### `git::patch`

`parse_patch(&str) -> Vec<FilePatch>` for `diff --git` text (renames, new /
deleted modes, binary markers, `\ No newline at end of file`, CRLF) and
`parse_hunks(&str)` for a single file's `patch` field. A hunk keeps its
`@@` header text (function context) and old/new start lines.

### Core: `code_assistant_core::pull_requests`

- **Repo resolution**: session → effective path → `origin` remote (else the
  first GitHub remote) → `RepoRef`. The session's own PR comes from the
  lifecycle snapshot (number) or a head-branch lookup.
- **Read cache**, one entry per (repo, number, part), with in-flight
  coalescing (concurrent identical reads share one request) and these TTLs:

  | Part | TTL |
  |---|---|
  | list | 30 s |
  | detail | 60 s (10 min once merged / closed) |
  | checks | 15 s |
  | activity (comments, threads, commits) | 60 s |
  | diff page | 60 s; keyed by head sha, per-commit diffs 10 min |
  | files viewed | 15 s |
  | viewer, candidates | 10 min |

- **Stale-while-revalidate**: a read returns the held value at once and,
  when it is past its TTL, refreshes in the background; the fresh value is
  announced with an app-scoped `UiEvent::PullRequestChanged {pr, part}` and
  the view reads it again. On a rate-limited / failed refresh the last good
  value (≤ 10 min) is kept.
- **Disk snapshots** for a cold start: the last detail + activity of the
  20 most recent PRs and the last list per repo, under
  `<config_dir>/pull-requests/`. Shown instantly, then revalidated.
- **Writes** invalidate the PR's parts and publish `PullRequestChanged`;
  ticking a file viewed only invalidates files-viewed (no diff refetch).
- The lifecycle sweep's `gh pr view` becomes a batched `PullRequestSummary`
  read through the same client.

### UI: right panel tab "Pull request"

Navigation: the tab shows the session's PR when it has one, otherwise the
repo's PR list; a back crumb returns to the list. Detail tabs: **Summary**,
**Timeline**, **Code** (T3's Guide tab, an agent-written review tour, is
out of scope for now).

- **Header**: repo · `#N ↗` in the state colour; editable title; author ·
  updated; `base ← head` (copy), files, `+A −D`; behind-by warning with
  "Update branch"; primary action (Merge with method / Ready for review /
  Auto-merge / state badge) with a two-click inline confirm; ⋯ menu
  (refresh, draft/ready, close/reopen, auto-merge on/off, merge method,
  maintainer edits, open on GitHub, copy link, delete branch). Collapses to
  one row when scrolled.
- **Summary** (GitHub's Conversation): reviewers (avatars, verdict rings,
  picker), labels (chips, picker), description (markdown, edit), checks
  (collapsed list, popover in the tab bar), comments with bot / resolved
  filters, newest-first toggle, 10 conversations at a time, comment cards
  (header strip, verdict badge, reactions, edit, body clipped at 240 px),
  threads with replies, collapsed resolved/dismissed cards.
- **Timeline**: opened, commits (click → Code tab scoped to the commit),
  grouped comments, review verdicts, review threads, merged/closed.
- **Code**: commit scope, files in tree order, hide tests, viewed counter,
  per-file sticky header with Viewed checkbox (shift-click range, ticking
  folds), dual-gutter diff with syntax highlighting, inline review threads
  (reply, resolve, load more, reactions, edit), threads not on the diff,
  new comments on a line or range → pending review (or → agent, as today's
  line comments), diff pages loaded as the end scrolls into view.
- **Composer**: floating button with the pending-comment count; Comment
  (post, close with comment) and Review (Comment / Approve / Request
  changes + pending comments).
- **Optimistic**: files viewed (overlay, writes batched 400 ms, serialized),
  reactions (pending map keyed by the host's reaction signature); list after
  merge/close. Everything else waits for the server, then refreshes.
- **Freshness**: checks poll every 45 s while pending (60 s otherwise) only
  while the tab is visible; a full revalidation when the window activates.
- **Colours**: open emerald, draft zinc, closed red, merged violet;
  approved emerald, changes requested red, pending checks amber.

### Look (from T3 Code, light theme)

- **List row**: state glyph left; `#281` muted + title (truncated) on line
  one with a checks / review glyph and `+1,933 −179` (green / red, mono)
  right-aligned; line two small and muted: avatar + login, repo, relative
  time right-aligned. Group headers ("Others 4") with an icon and a hairline
  to the right. Above the list: search field ("Search pull requests, or
  label:bug"), then outline buttons Sort, Filters, a refresh icon button.
  Sort menu: Merge readiness, Blocked on me, Recently updated, Newest,
  Oldest, Largest, Smallest. Filters menu with sub-menus showing the current
  value right-aligned: State (Open), Involvement (All), Author (Anyone),
  Labels (Any), Draft, Review, Checks (All).
- **Detail header**: muted `owner/repo` followed by `#290 ↗` in the state
  colour; right side an outline "Check out ▾", a state-coloured outline
  badge ("Merged" with merge icon) or the merge button, and `⋯`. Title in
  semibold, larger. Meta line: avatar + login (medium) · "updated 9h ago"
  muted, right-aligned mono `gh pr checkout 290` (click copies). Branch
  line in mono muted: `main ← feat/image-file-renderer`, right-aligned file
  icon "6 files" `+432 −46`. A hairline separates the header from the tab
  row.
- **Tab row**: segmented control (Summary · Timeline · Code), selected
  segment white with a soft shadow on a light grey track. Right side per
  tab: Summary "✓ 5 of 6 passing" (checks popover), Timeline "💬 0 · ⟜ 1 ·
  ⇅ Newest first", Code nothing (its toolbar follows below).
- **Summary**: label column (icon + "Reviewers", "Labels") with values
  ("None" muted + add icon); "Description ▾" section header; the body as
  rendered markdown at normal reading size.
- **Timeline**: vertical rail with 32 px markers (icons or avatars), text
  "Pull request merged / 9h ago", "<login> Pull request opened", commit rows
  with headline, short sha, time and `+432 −46` right-aligned.
- **Code**: toolbar "All commits ▾" (dropdown lists "All commits" and each
  commit headline + short sha), "6 files · 0 / 6 viewed"; right-aligned icon
  toggles (whitespace ¶, collapse all, stacked | split, wrap, file tree).
  File header rows: chevron, status icon (blue dot modified, green plus
  added), full path, `+69 −6`, "☐ Viewed". Diff: single line-number gutter,
  added lines with a green bar at the left edge and a pale green
  background, syntax highlighted; separators "30 unmodified lines" and "More
  unchanged context may be available" as centred muted text on a hairline.
- **⋯ menu**: Refresh; (agent items); Open on GitHub; Copy link ⇧⌘C; Copy
  PR number ⇧⌘K; Revert changes. Items have leading icons and an optional
  muted second line.
- **Composer**: round floating button bottom right with a speech-bubble
  icon.
