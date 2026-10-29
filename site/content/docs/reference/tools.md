---
title: Built-in tools
weight: 30
lede: The tools the agent can call, grouped by purpose. Tools from MCP servers come on top, named mcp__<server>__<tool>.
frontends: [app, tui, acp]
---

The **Read-only** column decides what the `write-tools` [permission tier](/docs/work/permissions/)
asks about: tools that aren't read-only need approval there.

## Files and projects

| Tool | Does | Read-only |
|---|---|---|
| `list_projects` | The configured projects | yes |
| `list_files` | Directory trees in a project | yes |
| `glob_files` | Files matching glob patterns | yes |
| `search_files` | Regex search across files, also inside documents | yes |
| `read_files` | Read files or line ranges | yes |
| `view_documents` | PDF, DOCX, XLSX, PPTX, ODT, RTF, HTML, XML, CSV, TSV as Markdown | yes |
| `view_images` | Look at image files (PNG, JPEG, GIF, WebP, BMP, TIFF, SVG) | yes |
| `write_file` | Create or overwrite a file | no |
| `edit` | Replace a unique piece of text in a file | no |
| `replace_in_file` | Diff-block edits (with `--use-diff-format` or `edit_format: diff`) | no |
| `delete_files` | Delete files | no |

## Commands

| Tool | Does | Read-only |
|---|---|---|
| `execute_command` | Run a shell command. With `background: true`, keep it running ([details](/docs/long/background/)) | no |
| `write_stdin` | Send input to, or read more output from, a background command | no |

## Web

| Tool | Does | Read-only |
|---|---|---|
| `web_search` | Search with DuckDuckGo | yes |
| `web_fetch` | Load a page as text | yes |
| `perplexity_ask` | Ask Perplexity (needs `perplexity_api_key`) | yes |

## Browser {#browser}

| Tool | Does | Read-only |
|---|---|---|
| `browser_navigate` | Load a URL, or go back/forward. Opens the browser | yes |
| `browser_read_page` | Accessibility tree with element refs | yes |
| `browser_find` | Tree lines matching a query | yes |
| `browser_get_page_text` | The page's main text | yes |
| `browser_computer` | Click, type, keys, scroll, hover, drag, wait, screenshot, zoom, record | no |
| `browser_form_input` | Set a field, checkbox or select | no |
| `browser_javascript` | Run JavaScript in the page | no |
| `browser_read_console_messages` | Console messages and exceptions | yes |
| `browser_read_network_requests` | Requests, or one response body | yes |
| `browser_resize_window` | Viewport presets, phone emulation, color scheme | no |
| `browser_tabs_context`, `_create`, `_select`, `_close` | Tabs | (varies) |
| `browser_batch` | Several actions back to back in one call | no |
| `browser_close` | Close a profile's browser | no |
| `browser_login` | Log in as you on a persistent profile ([details](/docs/work/browser/#login)) | yes |
| `browser_profiles` | Profiles and their last login | yes |

## Working with you and over time

| Tool | Does | Available in |
|---|---|---|
| `ask_question` | Ask up to four multiple-choice questions | App |
| `update_plan` | Keep the session's plan | All |
| `name_session` | Give the session a title | All |
| `spawn_agent` | Start a [sub-agent](/docs/long/sub-agents/), read-only or full, optionally in the background | All |
| `schedule_wakeup`, `cancel_wakeup` | [Timers](/docs/long/background/#wakeups) that continue the session | App, Terminal |
| `monitor`, `stop_monitor` | [Commands whose output lines become events](/docs/long/background/#monitors) | App, Terminal |
| `search_sessions`, `get_session_content` | Search and read past sessions | All |
| `list_skills`, `read_skill` | The [skills](/docs/configure/skills/) catalog and a skill's instructions | All |
