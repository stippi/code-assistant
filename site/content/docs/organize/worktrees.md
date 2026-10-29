---
title: Work on branches in worktrees
linkTitle: Worktrees
weight: 20
only: app
lede: Give a session its own git worktree, so several agents can work on different branches of the same repository at the same time.
frontends: [app]
---

Two agents editing the same checkout get in each other's way. A **worktree**
is a second checkout of the same repository on another branch. Each session
can work in its own.

## Switch a session to a worktree

The last dropdown in the selector row is the session's working directory:

{{< selectors on="⎇ Local" >}}<div class="menu"><div class="cur">Local<small>The project folder</small></div><div>feature/search<small>An existing worktree</small></div><div>New worktree...<small>Create one for a branch</small></div></div>{{< /selectors >}}

- **Local**: the project folder itself.
- An **existing worktree** of the repository.
- **New worktree...**: enter a branch name. If a worktree for that branch
  already exists, it is reused. Otherwise a new one is created next to the
  project, in `../.worktrees/<repo>/<branch>`.

From then on the session's tools (search, edits, commands) and its Review and
Files panels work in that directory.

## Why it helps

- Run a refactoring in one session and a bug fix in another, without
  conflicts.
- Review each branch on its own in **Review → Branch vs base**.
- Once a branch is merged, its session [settles](/docs/organize/sessions/#settle)
  by itself.

The session remembers its branch. Sessions also notice a branch when the agent
creates or checks one out itself (`git checkout -b …`), and the sidebar shows
it together with its pull request.
