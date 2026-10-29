---
title: Give it images, files and documents
linkTitle: Images and documents
weight: 70
lede: Paste screenshots into the message, point the agent at documents in the project, and keep a half-written message as a draft.
frontends: [app, tui, acp]
---

## Images in your message

{{< frontends >}}
{{< fe "app" >}}
Paste an image with <kbd>Cmd</kbd>/<kbd>Ctrl</kbd>+<kbd>V</kbd>. It shows as a thumbnail in the
composer's attachment row. Hover it to remove it. Models that accept images
get it with your message.
{{< /fe >}}
{{< fe "tui" >}}
Press <kbd>Ctrl</kbd>+<kbd>V</kbd> (or <kbd>Alt</kbd>+<kbd>V</kbd>) to paste an image from the clipboard. The usual
paste shortcut of your terminal (like <kbd>Cmd</kbd>+<kbd>V</kbd>) pastes text only.
{{< /fe >}}
{{< fe "acp" >}}
Attach images the way your editor allows (for example, paste them into Zed's
agent input). They reach the agent as image content.
{{< /fe >}}
{{< /frontends >}}

## Files the agent reads itself

Point the agent at files in the project by path, for example “look at
`docs/spec.pdf`”. It has tools for more than source code:

| Tool | Reads |
|---|---|
| `read_files` | Text files, in any encoding and with any line endings. Edits keep both as they were |
| `view_documents` | PDF, DOCX, XLSX, PPTX, ODT, RTF, HTML, XML, CSV, TSV, converted to Markdown |
| `view_images` | PNG, JPEG, GIF, WebP, BMP, TIFF, and SVG (drawn as PNG) |

Documents are also searchable. `search_files` finds text in Word, Excel,
PowerPoint and PDF files, too.

{{< callout note "Encodings and line endings" >}}
Code Assistant notices how each file is stored (encoding, BOM, CRLF or LF),
gives the model clean text, and writes the file back the same way. An edit
never silently turns your Windows line endings into Unix ones.
{{< /callout >}}

## Drafts

A message you started but didn't send is saved with its attachments for each
session. Switch sessions, close the app, come back later, and it is still
there.
