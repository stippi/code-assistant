---
title: Background jobs, monitors and wakeups
linkTitle: Background work
weight: 30
lede: The agent can start long-running work, go idle, and pick up again when something happens, without asking you to check back.
frontends: [app, tui]
---

Three tools let a session continue on its own. Each one delivers a short
message to the session. If the session is idle, the message starts a new turn.
If the agent is working, it gets the message at its next step.

## Background commands

For slow builds, dev servers, watchers and interactive programs, the agent
runs `execute_command` with `background: true`. The tool waits 5 seconds:

- If the command finished by then, the result is the same as a normal call.
- Otherwise the agent gets the output so far and a **session id**. It can
  send input with `write_stdin` and read more output.

When a background job exits, the session gets a message
`[background job <id> exited] …` with the exit status.

## Monitors

A **monitor** is a command whose output lines are events: a log tail, a
`watch` loop, a script that prints when a CI run finishes. The agent starts
one with `monitor`. Each new line, or a batch of lines, arrives as a
`[monitor #N] …` message. `stop_monitor` ends it.

> Run the test watcher as a monitor. Whenever a test fails, look into it and fix it.

## Wakeups

`schedule_wakeup` sets a timer, from one second up to seven days, with a
message the agent writes for its future self. When the timer fires, the session
gets that message and picks up the work again. `cancel_wakeup` removes a
timer.

> Deploy to staging, then check back in 15 minutes and look at the error rate.

## What you see

{{< frontends >}}
{{< fe "app" >}}
- A session waiting on a monitor or a wakeup is shown as *Monitoring* in the
  sidebar instead of looking finished. It doesn't settle automatically.
- Below the conversation, each running monitor has a line with a **Stop**
  button, and each wakeup shows a countdown.
- Notification messages are shown as compact lines (*Woke up*,
  *Background job finished*), not as your messages.
{{< /fe >}}
{{< fe "tui" >}}
Notifications are shown in the transcript when they arrive, and start a turn
when the session is idle.
{{< /fe >}}
{{< /frontends >}}

Wakeups and monitors last as long as the app or terminal is running. A wakeup
that was pending when you quit is shown as *Wakeup lost with a restart*.

The tools are in the **Terminal**, **Monitors** and **Wakeups**
[tool groups](/docs/configure/tools/), which you can switch off.
