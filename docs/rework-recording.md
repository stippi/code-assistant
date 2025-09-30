Provider-agnostic Recording/Playback Refactor

Goal
- Elevate the recording/playback mechanism from provider-specific (Anthropic) to an API provider-agnostic layer.
- Allow testing of provider implementations against previously recorded request/stream chunks for the same provider/API shape (e.g., OpenAI Responses).
- Enable UI playback modes (GPUI and Terminal UI) that follow recorded request+chunk sequences without starting backend agent loops.
- Minimize confusion by integrating this as a small, clear cross-cutting facility rather than a parallel subsystem.

Design Principles
- Recording and playback are orthogonal to providers and attach at the provider boundary.
- On record: capture sent request payload and the raw streaming SSE "data:" lines (plus coarse timing).
- On playback: bypass network and feed the captured SSE chunk stream back through the same parsing/streaming code paths for the target provider, preserving timing optionally.
- Avoid provider-specific record formats when possible; store raw chunks and a simple request envelope so the provider’s own parser is reused.
- Keep the feature optional and opt-in via CLI flags and the LLM factory.

Architecture Changes
1) llm::recording module
   - APIRecorder: unchanged core concept but made provider-agnostic; stores request JSON and a Vec<RecordedChunk> with timestamp_ms.
   - PlaybackState (NEW): loads a recording file (Vec<RecordingSession>) once, maintains current index, and provides next_session(). It also carries a fast boolean to drop timing delays.

2) llm::factory
   - Accepts optional record_path, playback_path, fast_playback.
   - If playback_path is provided, build a normal provider but inject PlaybackState where supported. This keeps provider selection unchanged, and playback replaces the HTTP call inside the provider.
   - If record_path is provided, set recorder on providers that support it so they capture request+chunks.

3) Providers
   - OpenAI Responses: integrate recorder+playback cleanly so:
     - On streaming send: start recorder before HTTP, record each SSE line (data: ...), end recorder on finish.
     - On playback: skip HTTP and push recorded chunks through the existing SSE line handler to reuse parsing + StreamingCallback code. Respect fast_playback to reduce delays.
   - Anthropic/Vertex/AICore (existing recorders): migrate progressively to the same pattern (recorder at provider boundary, playback using provider’s SSE parser). Remove anthropic_playback.rs once parity exists.

4) UIs (future step)
   - Add a UI mode (GPUI/Terminal) where the session is created from a recording file. Messages are not sent to backend agent loops; instead, the UI iterates through RecordingSession entries and displays as if streaming from the provider.
   - Provide CLI flags to launch in playback: e.g., --llm-playback <path> [--fast].

Recording File Format
- JSON array of RecordingSession objects
  - request: serde_json::Value (the provider-ready body payload)
  - timestamp: ISO-8601
  - chunks: [{ data: String, timestamp_ms: u64 }]
- This mirrors the existing Anthropic recorder and generalizes for all SSE-based providers.

Testing Strategy
- Unit tests for PlaybackState (load, next_session sequencing).
- Provider-level tests: given a fixture recording, validate that playback produces identical ContentBlocks and StreamingChunks as a live run (minus timestamps).
- CLI/manual: run with --llm OpenAIResponses --playback path/to.json and verify UI shows identical streaming.

Backwards Compatibility
- Target minimal confusion: use the existing factory knobs (record_path, playback_path, fast_playback).
- Deprecate anthropic_playback.rs after OpenAI Responses (and other providers) adopt the new pattern.

Work Plan
1) Baseline survey (DONE)
   - Located Anthropic recorder + playback and the Anthropic-specific player (anthropic_playback.rs).
   - Identified factory integration points and OpenAI Responses provider.

2) Core recording/playback API (PARTIALLY DONE)
   - recording.rs: keep APIRecorder; add PlaybackState (DONE).
   - Define a simple session iterator API (next_session) and fast flag (DONE).

3) Factory integration (DONE for OpenAI Responses entry wiring)
   - If playback_path provided: load PlaybackState and pass to the provider (OpenAI Responses for now). If record_path provided: attach recorder.
   - Left TODOs for AiCore/Anthropic/Vertex to adopt the same pattern.

4) Provider retrofit: OpenAI Responses (DONE)
   - Imports updated to include APIRecorder and PlaybackState (DONE).
   - Added fields to OpenAIResponsesClient: recorder: Option<APIRecorder>, playback: Option<PlaybackState> (DONE).
   - Added builder methods: with_recorder(path), with_playback(state) (DONE).
   - In send_with_retry: if playback is Some, call playback_request() and skip HTTP (DONE).
   - Implemented playback_request(): pulls next session, iterates chunks, optionally sleeps (17ms for fast mode or respects original timing), feeds chunks as "data: ..." lines into existing process_sse_line(), emitting StreamingChunk callbacks (DONE).
   - In try_send_request(): added recorder.start_recording(request_json.clone()) before HTTP; during streaming, recorder.record_chunk(data) for each SSE line; recorder.end_recording() on completion (DONE).
   - Non-streaming path updated to record entire response body and handle recorder lifecycle (DONE).
   - All existing tests pass, validating backward compatibility (DONE).

5) Remove anthropic_playback.rs (DONE)
   - After AnthropicClient has playback_state and we validate parity using the Anthropic provider’s own SSE parser, delete anthropic_playback.rs and route playback through the client itself.

   - AnthropicClient extended with playback support using the common streaming infrastructure (DONE).
   - Eliminated duplicated SSE processing logic by using shared ChunkStream abstraction (DONE).
   - Factory integration updated to wire playback_state to AnthropicClient (DONE).
   - anthropic_playback.rs removed successfully (DONE).

6) Extend to other providers (TODO)
   - VertexClient, AiCoreClient: mirror the same recorder and playback behavior.
   - For providers that don’t use SSE or have different shapes, adjust capture to store the minimal sequence the parser understands (e.g., line JSON chunks).

7) UI integration (TODO)
   - Add CLI flags to code_assistant binary to enable playback without backend sessions.
   - GPUI/Terminal UI: wire a mode that consumes the recording sessions and displays streams, bypassing agent loop logic.

8) Validation (TODO)
   - cargo build/test across crates.
   - Add targeted unit tests for OpenAI Responses playback path (feed a short recording fixture).
   - Manual run with sample recordings for sanity.

Open Questions / Notes
- Cross-provider portability is not required; recordings are provider-specific due to different SSE schemas. This design reuses each provider’s parser to keep recordings useful across provider evolution.
- Long-term: factor a small shared SSE capture helper to reduce duplication across providers.
- Consider optional metadata in RecordingSession (e.g., provider name, model) for audit clarity.

Current Status Snapshot
- streaming.rs: common ChunkStream abstraction with HttpChunkStream and PlaybackChunkStream implementations (DONE).
- recording.rs: PlaybackState implemented; APIRecorder unchanged.
- factory.rs: accepts playback_path/fast_playback; wires PlaybackState + recorder to OpenAI Responses and AnthropicClient; leaves TODOs for VertexClient and AiCoreClient.
- openai_responses.rs: fully implemented using common streaming infrastructure - no duplicated SSE processing logic.
- anthropic.rs: fully implemented using common streaming infrastructure - reuses existing SSE processing logic for both live and playback.
- anthropic_playback.rs: removed (deprecated in favor of provider-integrated playback).
- No UI changes yet.
