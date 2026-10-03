# Session lifecycle and the sidebar inbox

The GPUI sidebar is an inbox, not a file tree. It answers "what wants
something from me" first and "where are my sessions for project X" second.
This document describes the model behind it and where the pieces live.

## Model

Two questions are kept apart on purpose.

**What is the session doing?** The `SessionStatus`
(`code_assistant_core::session::lifecycle`) is derived from the live
`SessionActivityState` plus whether a tool permission request is open:

| Status             | Derived from                                   | Row treatment            |
|--------------------|------------------------------------------------|--------------------------|
| `NeedsApproval`    | an open `RequestToolPermission`                | warning icon and label   |
| `Working`          | `AgentRunning`, `WaitingForResponse`           | spinner, recedes         |
| `RateLimited`      | `RateLimited { .. }`                           | spinner, recedes         |
| `Failed`           | `Errored { .. }`                               | danger icon and label    |
| `RunningElsewhere` | `RunningExternally` (foreign agent lock)       | lock icon, recedes       |
| `Ready`            | `Idle`                                         | no label                 |

`Ready` is the unlabelled resting state: an idle agent waiting on the user,
whether it finished, asked a question, or failed to start. Colour is
reserved for "act now" (approval), "in motion" (working) and "broken"
(failed).

**Does it need me?** A session is *unread* when its `updated_at` is newer
than the user's last visit. Visits are recorded by
`SessionService::load_session` (showing a session is visiting it) and by
the frontend when the viewed session's agent goes idle under the user's
eyes. A session that was never visited counts as read, so the mark means
something after an upgrade. An unread ready row keeps a strong title and a
dot; a read ready row recedes.

`SessionStatus::should_recede` combines the two: busy rows recede because
they need nothing, ready rows recede once read, approval and failure never
recede.

## Settlement

Finished work leaves the inbox by *settling* into a collapsed shelf. Nothing
is deleted and a settled session comes back on request (hover action
"Un-settle"). Three routes:

- **Manual** — the row's hover action "Settle".
- **Inactivity** — no activity (`updated_at`) for
  `auto_settle_after_days` days; default 14, `0` turns the rule off.
- **Branch merged** — the session works on a branch (see below) and that
  branch is merged: either the host reports its pull request as merged, or
  `git::GitRepository::is_branch_merged` finds it merged locally. The local
  check recognises fast-forward and rebase merges by ancestry and squash
  merges by comparing the branch's diff against the base patch-wise
  (`git cherry`). The base is `origin/HEAD`'s target when the remote
  declares one, otherwise `origin/main`, `origin/master`, `main`, `master`.
  A session on the base branch itself never settles by this rule.

Busy sessions never settle. **Un-settling** records `unsettled_at`; the
automatic rules then wait for activity newer than that moment, so a session
the user deliberately keeps does not sink again the next day. An un-settled
session re-anchors at the top of the inbox.

The rules are evaluated by `SessionService::sweep_lifecycle`, run by
`lifecycle::run_lifecycle_sweeper` at startup and every ten minutes while a
frontend is open. The sweep collects candidates under the session lock,
refreshes pull requests and runs the git checks outside it, then settles
through the normal lifecycle update. Several processes sweeping at once is
harmless: writes are idempotent and locked.

## Branch and pull request

A session is associated with a branch in two ways:

- **Explicitly**, when it is switched to or created in a worktree
  (`SessionManager::set_session_worktree` sets `SessionConfig::branch`).
- **By observation**, after a run: `record_observed_branch` reads the branch
  checked out in the session's project and keeps it when the session has
  none yet and it is not the base branch. So a session that ran
  `git checkout -b feature/x` in the main checkout learns its branch at the
  end of that run. The first observed branch sticks; the user switching the
  checkout later does not relabel old sessions.

`ChatMetadata::branch` mirrors the config field for the sidebar.

The pull request behind the branch is read with the GitHub CLI
(`gh pr view <branch> --json …`, `session::pull_request`) and stored as a
`PullRequestSnapshot` (number, url, title, open/draft/merged/closed, review
decision, checks verdict) in the lifecycle record. It is refreshed after
each run of the session and by the sweep for every unsettled session on a
branch. Without `gh`, or without a login, sessions show only their branch.
The `fetch_pull_request` seam is the place to swap in an API client
(octocrab) later; what that needs on top is a token and the owner/repo
parsed from the remote.

## Storage

Lifecycle records never touch the session file: a visit must not rewrite a
multi-megabyte conversation. They live in `sessions/lifecycle.json`, a map
from session id to `SessionLifecycle`, guarded by `lifecycle.lock`, with
the same read-modify-write discipline as `metadata.json`
(`FileSessionPersistence::update_lifecycle`). Deleting a session removes
its record. The rules live in `<config_dir>/lifecycle.json`
(`LifecycleConfig`), editable under Settings → General.

## Event flow

- `UiEvent::UpdateSessionLifecycle { session_id, lifecycle }` is published
  for every change and forwarded to the sidebar regardless of which session
  is viewed.
- `SessionService::list_session_lifecycles` delivers the full map alongside
  `list_sessions` when the frontend refreshes its list.
- `RequestToolPermission` is now kept per asking session in the GPUI
  frontend whichever session is viewed, so the sidebar can flag it; the
  prompt itself still renders only in the asking session.

## Sidebar layout (GPUI)

```
Sessions                      [+]   ← header; "+" opens the project picker
  ⛉ Title                      2m
    project · Needs approval
  ◌ Title                     14m
    project · Working
  ⑂ Title                    ●  1d    ← open pull request, unread
    project · #220 feature/x · approved
  ⌥ Title                        3d    ← branch without a pull request
    project · feature/y
▸ Settled (12)
```

The inbox order is static: newest first by creation time, re-anchored only
when a session is un-settled. Activity changes emphasis, not position. The
settled shelf orders by settlement time. Projects are not a structure of
the list: every row names its project, and the header's "+" opens a
searchable picker (projects most recently active first, then "No project",
then "Add project…") that starts a session where it is chosen. A project
scope filter was tried and dropped: a filter that stays on hides exactly
the cross-project attention the inbox exists for.

The left column shows the status glyph while the agent is busy or blocked
(shield: approval, alert: failed, spinner: working, lock: elsewhere) and
otherwise the git glyph: pull request open (green), draft (grey), merged
(violet), closed (red), or a plain branch (violet). Clicking a pull request
glyph opens it. The subtitle names the project, then the status, or the
branch with its pull request number and what the pull request waits for
(checks failing, changes requested, approved). An unread row shows a dot
before the date.

## Deferred

- Snooze, pinning and manual reordering.
- Saving a temporary project to projects.json has no UI since the project
  rows left the sidebar; `SessionService::persist_project` remains.
- Per-session opt-out from automatic settlement (un-settle covers the
  common case).
- Only GitHub pull requests; GitLab and others show the branch alone.
- A branch observed after a run is never replaced; a session that moves to
  a second branch keeps the first.
- The terminal frontend lists sessions as before; it reads the same
  lifecycle data if it wants to.
