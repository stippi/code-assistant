---
title: Let it test in a real browser
linkTitle: Test in a real browser
weight: 90
lede: The agent can drive a real Chromium to open your web app, click through it, read the console and network requests, and check that a fix works. In the app you can watch it live and take over at any time.
frontends: [app, tui, acp]
---

## What the agent can do

The browser tools read a page the way a screen reader does (an accessibility
tree with numbered elements). They act on those elements or on screenshot
coordinates, and take a screenshot only when needed. The main tools:

| Tool | Does |
|---|---|
| `browser_navigate` | Open a URL, or go back/forward. Starts the browser |
| `browser_read_page`, `browser_find`, `browser_get_page_text` | Read the page |
| `browser_computer` | Click, type, press keys, scroll, drag, take screenshots, record a short clip |
| `browser_form_input` | Fill fields, checkboxes and selects |
| `browser_javascript` | Run JavaScript in the page |
| `browser_read_console_messages`, `browser_read_network_requests` | Console output, errors, requests and responses |
| `browser_resize_window` | Viewport sizes, phone emulation, light/dark color scheme |
| `browser_login`, `browser_profiles` | Log in as you, on a profile that remembers the login |

The full list is in [Built-in tools](/docs/reference/tools/#browser). By default every session
gets a fresh, throwaway browser.

Try it:

> Start the dev server, open http://localhost:5173, add an item to the cart
> and check that the total updates. Look at the console for errors.

## Watch it live {#panel}

{{< frontends missing="The live view is only in the app. In other frontends the agent works in a headless browser and reports what it sees." >}}
{{< fe "app" >}}
Click **Browser** in the title bar. When the agent opens its first browser in
a session, an open right panel switches to it by itself. You see the page as
it changes, with tabs and the address bar.

- **Take control** to click and type in the page yourself. The address bar
  and back/forward/reload work then, too.
- **Hand back** to let the agent use the browser again.
{{< /fe >}}
{{< /frontends >}}

## Log in without sharing your password {#login}

For pages behind a login, the agent calls `browser_login` with a named
**profile**:

1. You are asked to approve the login handoff.
2. A visible browser window opens on the login page.
3. You log in there yourself, with your password, 2FA or certificate, and
   confirm.
4. The agent continues, logged in. It never sees what you typed.

The login is saved in the profile. Later sessions that use the same profile
start logged in until the site's session expires. `browser_profiles` lists
the profiles and when each was last logged in.

## Turn it off

The browser tools are a [built-in tool group](/docs/configure/tools/). Switch
them off for a session in the **Tools** menu, or for all new sessions in
**Settings → Tools**.
