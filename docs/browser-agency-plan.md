# Browser Agency Plan

This document plans an interactive, agent-driven browser capability on top of
the existing `web` crate. The goal is **browser agency**: the agent navigates,
clicks, types, and observes real web applications across many tool calls, with a
**human-in-the-loop login handoff** so the agent can reach authenticated state
("act as me") without the model ever seeing credentials, 2FA codes, or client
certificates.

Two consumer stories drive it:

- **code-assistant** tests web software under development (localhost, mostly no
  or trivial auth, ephemeral profiles fine).
- **pal** performs real tasks on real portals under the user's identity (e.g.
  the Elster tax portal), which requires persistent authenticated sessions and
  gated outward actions.

The engine is generic and lives upstream; both consumers register the same
tools. This matches the carve-out direction (generic tools shared, domain logic
per app).

## Where we are today

`web` (`crates/web/src/client.rs`) is a **read-only scraper**, not an agent:

- `WebClient` holds a single headless `Browser` plus a `reqwest` client.
- `search()` uses HTTP against DuckDuckGo — it doesn't touch the browser.
- `fetch()` opens a throwaway `new_page`, extracts the main content, converts
  HTML → Markdown, and **discards the page**. No click/type/screenshot, no page
  that survives across calls.
- The profile lives in a `TempDir` (`_user_data_dir`) that is **deleted on
  drop**. Cookies, localStorage, and session tokens do not survive the process.

Two structural facts block agency, and the second is the root cause of "I can't
log in as me":

1. **Stateless & headless.** One page per call, no interaction surface, no
   vision (screenshots).
2. **Ephemeral profile.** Even a successful login is wiped when the process
   ends.

Neither is a flaw — the crate was built for research/fetch. Agency is a new
capability alongside it.

## Design principles

1. **The model never authenticates.** It does not type passwords, TOTP codes, or
   handle the Elster certificate/ElsterSecure app. Authentication is always
   performed by the human, in a visible browser window. The agent only operates
   on the *resulting* authenticated session.
2. **Login is a rare event, not a per-run cost.** Persistent named profiles keep
   the session alive between runs; re-login happens only when the cookie
   actually expires.
3. **Same shape as PTY sessions.** An interactive browser session is a
   long-lived handle the agent drives across tool calls, held in an id-keyed,
   per-agent-session registry — exactly the `pty_session` pattern (see
   `docs/pty-sessions.md`).
4. **Actions with external effect are `outward`.** Reads/navigation/screenshots
   are `read_only`; submitting a form, filing a return, posting — anything a
   third party sees — is `outward`, so the existing permission tiers gate them
   with no new mechanism (see `docs/permission-tiers.md`).

## Architecture

### Engine (Layer 0)

Grow the `web` crate (it already owns `chromiumoxide`) with an interactive
session module — or add a sibling crate `browser_session` mirroring
`pty_session` one-to-one. Recommended: **extend `web`** to avoid a second
chromiumoxide dependency and keep the browser stack in one place.

```
BrowserSession          // one persistent Page/BrowserContext, survives tool calls
BrowserSessionManager   // id-keyed registry, LRU cap, one per agent session
BrowserProfile          // named, persistent user-data-dir + headful/headless policy
```

`BrowserSession` surface (all async, thin wrappers over chromiumoxide CDP):

| Method | Purpose | Tag |
|---|---|---|
| `navigate(url)` | go to a URL, wait for load | `read_only` |
| `screenshot()` | full-page or viewport PNG → vision content | `read_only` |
| `read()` | accessibility tree / trimmed DOM / text for the model | `read_only` |
| `click(selector \| coords)` | click an element | context-dependent |
| `type(selector, text)` | type into a field (never credentials) | context-dependent |
| `press(key)` | keyboard key (Enter, Tab, …) | context-dependent |
| `wait_for(condition)` | wait for selector / navigation / timeout | `read_only` |
| `eval(js)` | run JS in the page, return JSON | context-dependent |
| `login_handoff(url)` | headful, pause for the human, resume authenticated | `read_only` |
| `close()` | drop the session | `read_only` |

**Vision is mandatory for real portals.** Elster (and most rich SPAs) cannot be
driven reliably from DOM→Markdown; the model must *see* the page. `screenshot()`
returns image content into the transcript (the LLM layer already supports
multimodal input). DOM/AX-tree reads complement it for precise selectors.

### Profiles & persistence

Replace the `TempDir` with a **named, persistent profile directory**, e.g.
`~/.code-assistant/profiles/<name>` (pal: `$PAL_HOME/profiles/<name>`). A profile
carries cookies/localStorage/passkeys, so:

- Log in once (headful, human) → the `elster` profile is authenticated.
- Every later run with that profile starts already authenticated, until expiry.
- Profiles are portable: the same directory copied to another machine carries
  the session with it (see pal/NUC below).

Keep an **ephemeral profile** option (today's `TempDir` behavior) as the default
for code-assistant's throwaway test runs.

### The login handoff primitive

`login_handoff(url)` is the heart of "act as me". Flow:

1. Agent opens/ensures a **headful** window on the named profile and navigates
   to `url`.
2. Agent **pauses the turn** and emits a confirmation-style request through the
   existing `PermissionMediator` seam: *"Please log in in the browser window,
   then confirm."*
3. The **human** performs the full login in the real window — password, 2FA,
   certificate, whatever the site demands. The model sees none of it.
4. Human confirms (allow) → agent resumes in the same, now-authenticated page.
   Deny → agent aborts the flow.
5. The session cookie now lives in the persistent profile; subsequent runs skip
   the handoff until expiry.

This reuses the same seam as pal's outward-confirmation round-trip — it is a
"wait for the human at the browser" prompt rather than a yes/no, but travels the
same channel (TUI prompt port / Telegram inline keyboard).

### Tools (code_assistant_core)

Add `code_assistant_core::tools::impls::browser_*` tools, each a `DynTool` over
`BrowserSessionManager`, registered in `register_default_tools()`:

- `browser_open` (open/attach a session on a named profile), `browser_navigate`,
  `browser_screenshot`, `browser_read`, `browser_click`, `browser_type`,
  `browser_wait`, `browser_eval`, `browser_login_handoff`, `browser_close`.
- Capability tags per the table above; submit-style actions carry `outward`.

Because the tools are generic, **pal inherits them** through the shared registry
— no pal-specific tool code, only pal-specific profiles and gating.

### Power-user escape hatch: attach to an existing Chrome

chromiumoxide can `Browser::connect(ws_url)` to a running Chrome started with
`--remote-debugging-port`. This gives the agent the user's *real* browser with
every existing login, zero handoff. Offer it as an explicit opt-in (a profile
kind `Attach { ws_url }`), **not** the default — the agent then shares the live
browser and can act on everything, which is the wrong default for autonomous,
identity-bearing tasks. The dedicated persistent profile is safer and
reproducible.

## pal / NUC specifics

pal targets the NUC (replacing hermes). Key updated fact: **the NUC has a
monitor attached on demand — it is not permanently headless.** That collapses
the earlier "headless NUC" constraint:

- pal can run the **same `login_handoff` primitive locally on the NUC**: when
  auth is needed, pal opens the headful window on the NUC's display and notifies
  the user over Telegram (*"please come to the NUC and log in to Elster, then
  confirm"*), using the existing channel + confirmation seams. The user walks
  over, logs in, confirms via the Telegram inline keyboard (or a local confirm),
  pal resumes.
- **Profile transfer is now optional, not required.** If the user prefers, they
  can still do the one-time headful login on the laptop and copy the `elster`
  profile directory to the NUC — useful when nobody is at the NUC — but it is a
  convenience path, not the primary mechanism.
- **Outward gating already exists.** Filing the return (submitting on Elster) is
  `outward`; pal's outward-confirmation interceptor prompts before it fires. No
  new mechanism.

This also corrects an earlier assumption recorded for pal: **headless-CDP
browser use with a pre-authenticated profile is a good fit for the NUC**, unlike
full desktop computer use (mouse/keyboard/whole-screen), which was rightly
deferred. Browser agency and desktop computer use are different capabilities;
this plan covers only the former.

## Rollout (suggested checkpoints)

Following the repo's TDD/checkpoint working style — each step compiles, is
tested, and is committable on its own:

1. **Persistent profiles.** Add a profile abstraction to `web`; make the
   user-data-dir named/persistent with the `TempDir` path kept as the ephemeral
   default. Existing `fetch`/`search` behavior unchanged. Tests: profile dir
   created/reused; cookies survive a relaunch (spin a local axum server that
   sets a cookie, as in the existing dev-dependency).
2. **Interactive `BrowserSession` + `BrowserSessionManager`.** Long-lived page,
   navigate/read/screenshot/click/type/wait/eval/close; id-keyed registry with
   LRU, one per agent session, dropped with the agent session. Tests drive the
   local axum app.
3. **Browser tools.** Wire the `browser_*` `DynTool`s into
   `register_default_tools()` with capability tags; screenshots return image
   content. Tests via `tools::test_registry()`.
4. **Login handoff.** Headful mode + `browser_login_handoff` on the
   `PermissionMediator` seam (pause → human → resume). Verify end-to-end in
   code-assistant against a site that needs a manual login.
5. **pal wiring.** pal registers the tools, defines `$PAL_HOME/profiles/*`,
   routes the handoff prompt over Telegram, and tags Elster submit actions
   `outward`. Dogfood the Elster login-and-navigate loop on the NUC.

## Open questions

- **Selector strategy for the model:** screenshot-coordinate clicks vs.
  accessibility-tree node ids vs. CSS selectors. Likely a mix; the AX tree gives
  stable handles that pair well with vision. Decide during step 2.
- **Session-expiry detection:** how the agent recognizes it has been logged out
  (redirect to login URL, a known selector) and re-triggers `login_handoff`
  instead of flailing.
- **Handoff timeout policy on the NUC:** how long pal waits for the human before
  giving up / re-notifying — ties into the existing open question about
  unattended permission prompts blocking a lane's turn.
