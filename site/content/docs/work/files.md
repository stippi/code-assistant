---
title: Browse project files
linkTitle: Browse files
weight: 60
only: app
lede: The Files panel shows the session's project as a tree, next to a viewer that can render Markdown and SVG and lets you edit and comment.
frontends: [app]
---

Click **Files** in the title bar.

## The tree

- Folders load as you expand them.
- Type in the **filter** field to switch to a flat list of fuzzy matches over
  all files (`svc` finds `session/service.rs`).
- Choose **Show/Hide gitignored files** to include or hide ignored entries.
- Right-click an entry to **Copy Path**, **Copy Relative Path**,
  **Comment on File…** or **Delete File…**.
- Changes on disk show up on their own.

The open file, the expanded folders and the filter setting are remembered per
session.

## The viewer

The viewer is a small code editor with line numbers, syntax highlighting and
search. You can edit the file. It saves when you pause typing, when you open
another file, or right away with <kbd>Cmd</kbd>/<kbd>Ctrl</kbd>+<kbd>S</kbd>. If the file changes on disk while you
have unsaved edits, the viewer asks which version to keep.

Select lines to **comment** on them, just like in the [Review panel](/docs/work/review/).

## Rendered views

Some files can be shown in other ways than as text. A dropdown above the file
switches between them:

| Files | Views |
|---|---|
| Markdown (`.md`, `.markdown`, …) | **Markdown** (rendered like in the chat) or **Text** |
| SVG | the drawing (**Theme background** or **White background**) or **Text** |
| everything else | **Text** |

What you pick applies to every file of that kind and is remembered per session.
Pick **Text** for `.md` once, and Markdown files open as text from then on.
