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
- **Workflow approvals** (a fork's open pull request): the detail read
  lists the head's runs waiting for a maintainer
  (`actions/runs?head_sha&branch&event=pull_request&status=action_required`,
  up to 1000) and adds them as `action-required` checks
  (`Detail::workflow_approvals`; unknown after a failed read, shown as
  "Workflow approval status"); the checks revalidation probes a fork's
  `actions/runs?head_sha&event=pull_request` too. `Action::ApproveWorkflows`
  (writers only) refuses a head another open pull request shares
  (`PullRequestsByHead`) and reads the head again before each
  `POST actions/runs/{id}/approve`.
- **Stacks** (GitHub's stacked pull requests): `Client::stack` reads
  `stacks?pull_request=N` (ETag-revalidated; 404 = none) and then
  `stacks/{number}` for titles, drafts and heads; github.com listings mark
  where rows sit (`ListEntry::stack`, aliased `PullRequestStackMembership`,
  failing quietly). `merge_stack` sends `PUT pulls/N/merge-async` and polls
  `merge-async/{uuid}` (backoff to 10 s, 5 min); `stack_rebase` checks the
  stack still is what the viewer saw (`StackHead`s) and that every open
  branch may be pushed to (`PullRequestBranchAccess`), and returns what a
  rebase needs (remote URL, credential header, layers). The refusals keep
  T3 Code's words.
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
  first remote, by name, whose URL names a GitHub repository) → `RepoRef`
  (`git::GitRepository::remotes`, `pull_requests::github_remote`). The
  head branch is the session's `branch`, else the branch checked out there
  unless it is the base branch. The session's own PR comes from the
  lifecycle snapshot (number) or a head-branch lookup (newest PR not from a
  fork, cached 60 s).
- **Client**: one `PullRequests` store per `SessionService`, built from
  `AgentRuntimeOptions::pull_requests` (`PullRequestSource {client,
  snapshot_dir}`; `None` = `github::Client::new()` and
  `<config_dir>/pull-requests/`; tests pass a `FakeTransport` client). It
  sits in the service's `ServiceCtx`; every API method runs through
  `call_io`, outside the session lanes. Agent runs get the client through
  `RunConfig::github` for the post-run lifecycle refresh.
- **Read cache** (`pull_requests/cache.rs`), one LRU per part, keyed by
  the PR (lists by `(repo, query)`, diffs by `(pr, commit, cursor)`; a
  detail read that sees a new head drops the PR's diffs, so a diff
  prefetched alongside the first detail read still counts), with in-flight
  coalescing (concurrent identical
  reads share one request, run on a task of its own so it lands even when
  the readers are gone) and these TTLs:

  | Part | TTL | Held |
  |---|---|---|
  | list | 30 s | 64 |
  | detail | 60 s (10 min once merged / closed) | 128 |
  | checks | 15 s | 128 |
  | activity (comments, threads, commits) | 60 s | 128 |
  | diff page | 60 s; per-commit diffs 10 min | 128, 32 MB of patch text |
  | files viewed | 15 s | 128 |
  | stack | 60 s | 128 |
  | viewer, candidates | 10 min | 32 |
  | own PR of a branch | 60 s | 128 |

- **Stale-while-revalidate**: a value within its TTL is served as it is;
  up to 10 min past it, at once with `refreshing: true` while a background
  read (`Priority::Background`) refreshes it. When that read lands, stored
  or failed, an app-scoped `UiEvent::PullRequestChanged {pr, part}` (lists:
  `PullRequestListChanged {repo}`) tells the view to read again. A failed
  refresh keeps the value and waits one TTL before the next try (no refresh
  loop). With nothing servable the reader waits for an interactive read; if
  that fails for a passing reason (rate limit, network, 5xx), a value within
  the window is served instead.
- **Disk snapshots** for a cold start: the last detail + activity of the
  20 most recently read PRs and the first page of the 20 most recently read
  lists, one JSON file each under `<config_dir>/pull-requests/` (name = hash
  of the key, the key stored inside). Written on a background task, one
  write after the other, pruned after each write; read when memory holds
  nothing, shown at once as stale, then revalidated.
- **Writes** run interactively, then (failed or not) invalidate the parts
  they change: the next read waits for GitHub, reads under way are dropped
  so a pre-write answer never lands. Each part is announced with
  `PullRequestChanged`; a write touching the detail also invalidates the
  repo's lists and publishes `PullRequestListChanged`.

  | Write | Parts |
  |---|---|
  | comment, edit comment, reply, resolve, react | activity |
  | title / body / maintainer edits, labels | detail (+ label candidates) |
  | review, request reviewers | detail, activity (+ reviewer candidates) |
  | action (merge, close, update branch, …) | detail, checks, activity |
  | files viewed | files viewed only (no diff refetch) |
  | stack merge / rebase | every part but files viewed, of every layer |
  | refresh | every part, candidates |

- The lifecycle sweep reads the session PRs through the same client:
  one batched `PullRequestSummary` for all known PR numbers, a head-branch
  lookup for sessions without one (or whose PR now has another head or is
  gone), then one more batch for those (`session::pull_request`).
- **Checkout** (`pull_requests::checkout`, `git::pull_request`), as
  `gh pr checkout` does it: the head branch fetched from the remote of the
  pull request's repository; a fork's head as `refs/pull/N/head` from it
  (no remote added), tracking the fork's URL and branch when maintainers
  may push, else the pull request's ref; a fork's branch named like the
  default (or base) branch becomes `owner/branch`; an existing branch only
  fast-forwards. *Here* refuses tracked changes; *in a worktree* reuses the
  branch's worktree or adds one beside the project, and a new session for
  the project works in it. The session knows the pull request as its own
  (lifecycle snapshot).
- **Stack rebase** (`git::stack_rebase`, after T3's cascade): in a scratch
  repository each open layer is replayed onto the rebased one below and
  pushed with a lease on the head it had; the credential goes through
  `GIT_CONFIG_*` in the environment, never the command line.

#### API (`SessionService`, `session/service/pull_requests.rs`)

Model types are the `github` crate's (`github::model::*`, `RepoRef`,
`PullRequestRef`); core re-exports nothing of its own for them. `Fetched`,
`PullRequestPart` and `SessionPullRequests` live in
`code_assistant_core::pull_requests`. Errors are `anyhow` around
`github::Error` (downcast for e.g. `NoCredential`). List refreshes have no
method of their own: list reads revalidate after 30 s, and writes that
change a row invalidate the repo's lists.

```rust
/// A cached read: the value plus whether a background refresh is under way
/// (a `PullRequestChanged` follows when it lands).
pub struct Fetched<T> { pub value: T, pub fetched_at: SystemTime, pub refreshing: bool }

pub enum PullRequestPart { Detail, Checks, Activity, Diff, FilesViewed, Stack }

// Where a session's pull requests live and which one is its own; `None`
// without a project or a GitHub remote.
async fn session_pull_requests(&self, session_id: String) -> Result<Option<SessionPullRequests>>;
//   SessionPullRequests { repo: RepoRef, head_branch: Option<String>, own: Option<u64> }

// Reads: held value at once (memory, else disk snapshot) and a background
// revalidation when past the TTL; nothing held → waits for the fetch.
// list_stats, thread_comments and file_contents are not cached; viewer and
// candidates are cached but return the value alone.
async fn pull_request_list(&self, repo: RepoRef, query: ListQuery) -> Result<Fetched<ListPage>>;
async fn pull_request_list_stats(&self, prs: Vec<PullRequestRef>) -> Result<Vec<DiffStat>>;
async fn pull_request_detail(&self, pr: PullRequestRef) -> Result<Fetched<Detail>>;
async fn pull_request_checks(&self, pr: PullRequestRef) -> Result<Fetched<Checks>>;
async fn pull_request_activity(&self, pr: PullRequestRef) -> Result<Fetched<Activity>>;
async fn pull_request_diff(&self, pr: PullRequestRef, commit: Option<String>, cursor: Option<String>) -> Result<Fetched<DiffPage>>;
async fn pull_request_files_viewed(&self, pr: PullRequestRef) -> Result<Fetched<FilesViewed>>;
async fn pull_request_thread_comments(&self, pr: PullRequestRef, thread_id: String, cursor: String) -> Result<ThreadCommentsPage>; // after the thread's next_comments_cursor
async fn pull_request_file_contents(&self, pr: PullRequestRef, commit: Option<String>, change: FileChange, old_path: String, new_path: String) -> Result<DiffFileContents>;
async fn pull_request_reviewer_candidates(&self, pr: PullRequestRef) -> Result<ReviewerCandidates>;
async fn pull_request_label_candidates(&self, pr: PullRequestRef) -> Result<LabelCandidates>;
async fn pull_request_stack(&self, pr: PullRequestRef) -> Result<Fetched<Option<Stack>>>;
async fn github_viewer(&self, host: String) -> Result<Viewer>;

// Writes: run interactively, then invalidate and publish PullRequestChanged.
async fn pull_request_comment(&self, pr, body: String) -> Result<()>;
async fn pull_request_update_comment(&self, pr, id: String, kind: EditableCommentKind, body: String) -> Result<()>;
async fn pull_request_update(&self, pr, update: PullRequestUpdate) -> Result<()>;
async fn pull_request_submit_review(&self, pr, review: ReviewSubmission) -> Result<()>;
async fn pull_request_reply(&self, pr, thread_id: String, body: String) -> Result<()>;
async fn pull_request_resolve_thread(&self, pr, thread_id: String, resolved: bool) -> Result<()>;
async fn pull_request_react(&self, pr, subject_id: Option<String>, content: ReactionContent, reacted: bool) -> Result<()>;
async fn pull_request_request_reviewers(&self, pr, reviewers: Vec<Reviewer>, requested: bool) -> Result<()>;
async fn pull_request_set_labels(&self, pr, labels: Vec<String>, applied: bool) -> Result<()>;
async fn pull_request_set_files_viewed(&self, pr, files: Vec<(String, bool)>) -> Result<()>;
async fn pull_request_action(&self, pr, action: Action) -> Result<()>; // incl. ApproveWorkflows
async fn pull_request_stack_action(&self, pr, stack_number: u64, expected: Vec<StackHead>, action: StackAction) -> Result<()>;
async fn pull_request_checkout(&self, session_id: String, pr, place: CheckoutPlace) -> Result<PullRequestCheckout>; // Here | Worktree
async fn pull_request_refresh(&self, pr: PullRequestRef) -> Result<()>; // drop every cached part, refetch on next read
```

`UiEvent::PullRequestChanged { pr: PullRequestRef, part: PullRequestPart }`
and `UiEvent::PullRequestListChanged { repo: RepoRef }` are app-scoped
(always forwarded); a view holding that PR re-reads the part.

### UI: right panel tab "Pull request"

Navigation: a row of tabs (T3's right panel tabs): the repo's PR list,
then a tab per opened PR ("#290" after its state glyph). Opening an open
PR shows its tab; closing the shown one shows its right neighbour, else the
last, else the list; close by the button replacing the glyph under the
pointer, a middle click or the context menu (others, to the right, all);
drag to reorder; overflow scrolls with arrows. A tab keeps its page (scroll,
sub-tab, drafts) while open; only the shown page reads and polls. Open tabs
and their sub-tabs are remembered per session; without any, the session's
own PR opens. Detail tabs: **Summary**, **Timeline**, **Code** (T3's Guide
tab, an agent-written review tour, is out of scope).

- **Header**: repo · `#N ↗` in the state colour; editable title; author ·
  updated; `base ← head` (copy), files, `+A −D`; behind-by warning with
  "Update branch"; primary action (Merge with method / Ready for review /
  Auto-merge / state badge) with a two-click inline confirm; ⋯ menu
  (refresh, draft/ready, close/reopen, auto-merge on/off, merge method,
  maintainer edits, open on GitHub, copy link, delete branch). Collapses to
  one row when scrolled. "Check out ▾": in a separate worktree (a new
  session on it, shown at once) or in this repository, or copy the `gh`
  command; the page says where it landed (T3's toasts as a notice).
  Stacks: "⧉ 2/3" with the stack's menu (layers top first, a click opens
  that PR's tab, the base, Merge stack (k) / Rebase stack) and "Merge
  stack", both confirmed over the panel; while a PR is in a stack (or its
  stack is not known yet) the single merge controls stay away; "Retry
  stack lookup" after a failed read.
- **Summary** (GitHub's Conversation): reviewers (avatars, verdict rings,
  picker), labels (chips, picker), description (markdown, edit), checks
  (collapsed list; popover in the tab bar: attention, running, then
  completed behind "Show all", "Details" links; "Approve workflows to run"
  in its place while a fork's runs wait, two clicks), comments with bot / resolved
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
  Toolbar toggles (remembered): hide whitespace changes (default on;
  client-side like T3: each hunk re-diffed without whitespace, per page
  off the UI thread), stacked | split (old left, new right, paired runs;
  new and deleted files stay one column), wrap (off: rows scroll sideways
  per file, line numbers stay), hide tests, file tree; narrow toolbars fold
  the first three into a menu. Gaps between hunks and the unknown tail
  expand by 100 lines or whole (shift-click whole); the first click reads
  the file (`pull_request_file_contents`, one read per file at a time) and
  highlights both whole sides. The file tree (right, min(320 px, 40 %))
  compacts single-folder chains, tints files by change with their letter,
  and a click unfolds the file and scrolls to it.
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
