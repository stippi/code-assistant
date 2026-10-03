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
- **Branch merged** — the session was switched to a branch
  (`SessionConfig::branch`, mirrored as `ChatMetadata::branch`) and that
  branch is merged into the repository's base branch.
  `git::GitRepository::is_branch_merged` recognises fast-forward and rebase
  merges by ancestry and squash merges by comparing the branch's diff
  against the base patch-wise (`git cherry`). The base is `origin/HEAD`'s
  target when the remote declares one, otherwise `origin/main`,
  `origin/master`, `main`, `master`. A session on the base branch itself
  never settles by this rule.

Busy sessions never settle. **Un-settling** records `unsettled_at`; the
automatic rules then wait for activity newer than that moment, so a session
the user deliberately keeps does not sink again the next day. An un-settled
session re-anchors at the top of the inbox.

The rules are evaluated by `SessionService::sweep_settlement`, run by
`lifecycle::run_settlement_sweeper` at startup and every ten minutes while a
frontend is open. The sweep collects candidates under the session lock, runs
the git checks outside it, then settles through the normal lifecycle update.
Several processes sweeping at once is harmless: writes are idempotent and
locked.

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
Sessions                      [+]   ← header; "+" starts a session in the
                                       scoped project, else the selected
  ● Title                      2m      session's project
    project · Needs approval
  ◌ Title                     14m
    project · Working
  Title                        1d
    project
▸ Settled (12)
▾ Projects                    [+]
  ▸ code-assistant     3   (hover: pin, +)
  ▸ lunar-walk         1
```

The inbox order is static: newest first by creation time, re-anchored only
when a session is un-settled. Activity changes emphasis, not position. The
settled shelf orders by settlement time. Clicking a project row scopes the
inbox and the shelf to that project (click again, or the header's "×", to
clear); the row's "+" starts a session there.

## Deferred

- Snooze, pinning and manual reordering.
- Per-session opt-out from automatic settlement (un-settle covers the
  common case).
- Sessions on the main worktree have no `branch`, so only worktree
  sessions see the merge rule.
- The terminal frontend lists sessions as before; it reads the same
  lifecycle data if it wants to.
