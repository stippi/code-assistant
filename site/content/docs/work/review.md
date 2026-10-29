---
title: Review and comment on the agent's changes
linkTitle: Review changes
weight: 50
only: app
lede: The Review panel shows what the agent changed, file by file, and updates while it works. Comment on any line, and the comment goes with your next message.
frontends: [app]
---

## Open the Review panel

Click **Review** in the title bar. The panel lists every git repository in
the session's working directory. Each repository has a section with its
changed files stacked one below the other, and each file has a header (path,
status, `+`/`−` counts) with its diff hunks below it. Click a file header to
collapse it.

Choose what to compare in the **Compare** dropdown:

- **Working tree**: uncommitted changes, the usual view while the agent works.
- **Branch vs base**: everything on the current branch compared with a base
  branch that you pick in a second dropdown. Use it to review a feature
  branch before you open a pull request.

The panel watches the repository. When the agent (or you) changes a file,
the diff updates on its own without flickering.

## Comment on lines

1. Select one or more lines in a diff. A small menu appears next to the
   selection.
2. Choose **Add comment**, write it, and save.
3. The comment becomes a chip in the composer's attachment row, a preview
   for one comment and a count for several.

When you send the message, the comments go with it, each with its file, its
lines and the commented code. Click the chip to list the comments, jump to
one, or remove it.

{{< callout tip "Review like a pull request" >}}
Let the agent finish, switch to **Branch vs base**, and go through the
files. Comment wherever something needs to change, then send a single
message: “Please address my comments.”
{{< /callout >}}

You can also comment on:

- **a whole file**: right-click the file in the Files view and choose
  **Comment on File…**.
- **a passage in the conversation**: select text in a message and choose
  **Comment** (or **Copy**).

## Other ways to look at changes

- The agent's edit cards in the conversation show each edit as a diff.
- In the terminal and in ACP editors, use your usual tools (`git diff`, the
  editor's diff view).
