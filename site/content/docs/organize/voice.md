---
title: Talk to all your sessions by voice
linkTitle: Voice mode
weight: 30
only: app
lede: A voice assistant that sits next to your sessions. Ask what's running, start new work, or send a message to a session, all without typing.
frontends: [app]
---

Voice mode is one assistant for the whole app, not tied to one session. It
uses a realtime speech model (OpenAI Realtime protocol) and only has tools for
working with conversations:

| Tool | Does |
|---|---|
| `list_conversations` | Which sessions exist, what state they're in (running, idle, waiting for you) |
| `list_projects` | Your projects, so it can start sessions in the right one |
| `message_conversation` | Send a message to a session, or create a new one with a first message |
| `get_conversation` | The latest answer of a session, or a short history |

Things you can say:

- “What's running right now?”
- “Start a conversation in the web-shop project that fixes the flaky login test.”
- “Tell the refactoring session to also update the docs.”
- “What did the build session end with?”

## Set it up

1. In **Settings → Voice**, pick a **Realtime provider**. It uses the
   credentials of a provider you already configured, for example OpenAI.
2. Choose the **Model** and a voice. Optionally choose a **Transcription model**
   for a written transcript, and the **Turn detection** eagerness.
3. Click the microphone in the title bar to **Start voice mode**.

The title bar shows what it's doing (*Listening*, *Hearing you*, *Speaking*).
You can **Mute** it, and open the transcript.

## Notifications

When a session finishes or needs you, the voice assistant tells you. It never
talks over you or over itself. It waits until it has finished speaking, then a
short quiet period (**Quiet time before notifications**). With **Only report
conversations started or messaged by voice**, it stays quiet about
everything else.

The settings are stored in `voice.json`:

```json
{
  "provider": "openai",
  "model": "gpt-realtime",
  "voice": "marin",
  "transcription_model": "gpt-4o-mini-transcribe",
  "vad_eagerness": "auto",
  "cooling_ms": 1500,
  "notify": "all"
}
```

On macOS, audio uses the system's voice processing with echo cancellation,
so you can use speakers instead of headphones.
