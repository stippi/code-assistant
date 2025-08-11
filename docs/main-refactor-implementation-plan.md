# main.rs Refactor Implementation Plan

This plan restructures crates/code_assistant/src/main.rs into a thin orchestrator while preparing for:
- Configurable, per-session LLM provider+model selection (persisted and restorable)
- Terminal mode session management (list/switch/new/delete) and optional concurrent sessions
- Clean separation of GPUI main-thread UI and background async backend
- A reusable LLM client factory in the llm crate
- Simpler CLI defaults and logging

The steps are written for an LLM to execute using our repository tooling. Follow the phases in order, validating after each.


## High-level goals
- Reduce main.rs to argument parsing + high-level dispatch
- Move provider construction to llm::factory
- Extract GPUI backend event handling into its own module, with helper utilities to avoid duplication
- Normalize CLI defaults to avoid unnecessary Option<T>
- Enable saving/loading the chosen LLM provider configuration in session persistence


## Constraints and notes
- GPUI must run on the main thread; backend runs off the main thread
- Keep terminal flags for record/playback and allow future runtime provider selection from UI
- Prefer Arc<dyn UserInterface> over Arc<Box<dyn UserInterface>> to simplify trait object usage
- Maintain existing functionality during refactor; feature additions can be guarded behind changes that preserve current behavior where possible


## Resulting module layout (target)
- crates/code_assistant/src/
  - main.rs (thin)
  - cli.rs (Args/Mode parsing + defaults)
  - logging.rs (setup_logging)
  - app/
    - terminal.rs (terminal runner; state loading; interactive loop)
    - gpui.rs (gpui runner; spawns backend; minimal logic)
  - ui/gpui/
    - backend.rs (handle_backend_events and helpers for each BackendEvent)
  - utils/
    - content.rs (content_blocks_from for attachments)
- crates/llm/src/
  - factory.rs (LLMProviderType and LLMClientConfig move here; create_llm_client)


## Phase 0 — Index and baseline validation
1) Inspect current main.rs, note public types/functions to be moved:
   - Args, Mode, LLMProviderType, LLMClientConfig, create_llm_client, setup_logging, run_mcp_server, run_agent_terminal, run_agent_gpui, handle_backend_events, run_agent


## Phase 1 — CLI and logging extraction + defaults cleanup
Goal: Move CLI and logging setup to dedicated modules; simplify Options when defaults exist.

Changes:
- Create crates/code_assistant/src/cli.rs with:
  - pub enum Mode { Server { verbose: bool }, }
  - pub enum LLMProviderType { AiCore, Anthropic, Groq, MistralAI, Ollama, OpenAI, OpenRouter, Vertex }
  - pub struct Args with simplified types:
    - path: PathBuf (with default_value = ".")
    - task: Option<String>
    - ui: bool
    - continue_task: bool
    - verbose: u8 using ArgAction::Count (accept -v, -vv, etc.)
    - provider: LLMProviderType (with default_value = "anthropic")
    - model: Option<String>
    - base_url: Option<String>
    - aicore_config: Option<PathBuf>
    - num_ctx: usize (with default_value_t = 8192)
    - tool_syntax: ToolSyntax (with default_value = "native")
    - record: Option<PathBuf>
    - playback: Option<PathBuf>
    - fast_playback: bool
    - use_diff_format: bool
  - impl Args { pub fn parse() -> Self } using clap::Parser
  - Provide From<u8> -> log level mapping if needed later

- Create crates/code_assistant/src/logging.rs:
  - pub fn setup_logging(verbose: bool, to_stdout: bool)
  - Use tracing_subscriber::EnvFilter to allow RUST_LOG override; default to a compact filter
  - Default to stderr unless explicitly to_stdout

- Update main.rs to use cli::Args and logging::setup_logging

Validations:
- Update uses of args fields: remove .unwrap_or_else and .unwrap_or for fields that now have concrete defaults
- cargo build
- search for old type paths that moved (LLMProviderType if kept here temporarily)


## Phase 2 — Introduce llm::factory with LLMProviderType and client creation
Goal: Move LLM client creation out of code_assistant main and centralize in llm crate.

Changes in crates/llm:
- Add crates/llm/src/factory.rs with:
  - pub enum LLMProviderType { AiCore, Anthropic, Groq, MistralAI, Ollama, OpenAI, OpenRouter, Vertex }
    - Keep existing variants identical for compatibility
  - pub struct LLMClientConfig {
      pub provider: LLMProviderType,
      pub model: Option<String>,
      pub base_url: Option<String>,
      pub aicore_config: Option<PathBuf>,
      pub num_ctx: usize,
      pub record_path: Option<PathBuf>,
      pub playback_path: Option<PathBuf>,
      pub fast_playback: bool,
    }
  - pub async fn create_llm_client(cfg: LLMClientConfig) -> anyhow::Result<Box<dyn LLMProvider>>
    - Move the entire provider match from main.rs here
    - Keep playback short-circuit semantics
    - Defer recorder wrapping per-provider if that’s how current clients work

Crate API adjustments:
- Export factory publicly in llm/lib.rs: pub mod factory;
- If LLMProviderType previously lived in code_assistant, consider re-exporting it for convenience in code_assistant: use llm::factory::LLMProviderType

Changes in code_assistant:
- Remove local LLMProviderType and LLMClientConfig definitions
- Import llm::factory::{LLMProviderType, LLMClientConfig, create_llm_client}

Interface change follow-ups:
- search_files for LLMProviderType and LLMClientConfig usages in code_assistant to update imports

Validations:
- cargo check


## Phase 3 — Thin main.rs orchestrator
Goal: Reduce main.rs to parsing args, then dispatch to server/terminal/gpui runners.

Changes:
- Create directory crates/code_assistant/src/app/
- Move run_agent_terminal into app/terminal.rs as pub async fn run(...)
  - Signature uses concrete types from cli and llm::factory
  - Internally: setup persistence, create Agent, load_or_init state, maybe_add_new_task, interactive_loop
  - Extract helper fns:
    - fn load_or_init_agent_state(...)
    - async fn maybe_add_new_task(...)
    - async fn interactive_loop(...)
- Move run_agent_gpui into app/gpui.rs as pub fn run(...)
  - This must stay sync because gpui.run_app() blocks main thread
  - It spawns the background runtime/thread, sets up GUI/backend communication, then runs the app
- Keep run_mcp_server as-is or move to app/server.rs; expose pub async fn run(verbose: bool)

- main.rs now:
  - use cli::Args
  - let args = Args::parse();
  - match args.mode { Some(Mode::Server{ verbose }) => app::server::run(verbose).await, None => { logging::setup_logging(...); if args.ui { app::gpui::run(args_to_cfg); } else { app::terminal::run(args_to_cfg).await } } }

Validations:
- cargo check
- Verify main.rs imports are minimal and compile


## Phase 4 — Extract GPUI backend event handling into ui/gpui/backend.rs
Goal: Move handle_backend_events and duplicated logic helpers into a dedicated module, reducing complexity in app/gpui.rs.

Changes:
- Create crates/code_assistant/src/ui/gpui/backend.rs with:
  - pub async fn handle_backend_events(
      backend_event_rx: async_channel::Receiver<ui::gpui::BackendEvent>,
      backend_response_tx: async_channel::Sender<ui::gpui::BackendResponse>,
      multi_session_manager: Arc<Mutex<crate::session::SessionManager>>,
      cfg: Arc<llm::factory::LLMClientConfig>,
      gui: ui::gpui::Gpui,
    )
  - Extract per-event helpers to keep the match small:
    - async fn handle_list_sessions(...)
    - async fn handle_create_session(...)
    - async fn handle_load_session(...)
    - async fn handle_delete_session(...)
    - async fn handle_send_user_message(...)
    - async fn handle_queue_user_message(...)
  - Each helper should:
    - Minimize lock hold times (lock, perform immediate action, drop lock before awaits)
    - Use shared utility to build content blocks for attachments
    - Use create_llm_client(cfg.clone()) to instantiate providers as needed

- Create crates/code_assistant/src/utils/content.rs with:
  - pub fn content_blocks_from(message: &str, attachments: &[persistence::DraftAttachment]) -> Vec<llm::ContentBlock>
  - Move duplicate logic from SendUserMessage and QueueUserMessage branches into this helper

- Update app/gpui.rs to import ui::gpui::backend::handle_backend_events and pass Arc<LLMClientConfig> instead of many scalars

Validations:
- search_files for handle_backend_events and update imports and call sites
- cargo check


## Phase 5 — Trait object simplification for UserInterface
Goal: Replace Arc<Box<dyn UserInterface>> with Arc<dyn UserInterface> throughout.

Changes:
- Change Agent::new signature to accept Arc<dyn UserInterface> instead of Arc<Box<dyn UserInterface>> (in crates/code_assistant/src/agent/...)
- Update all call sites:
  - Terminal: let user_interface: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());
  - GPUI: Arc::new(gui.clone())
- Remove unnecessary Box wrappers where present

Reference search:
- search_files for Arc<Box<dyn UserInterface>> and Box<dyn UserInterface>

Validations:
- cargo check
- cargo test


## Phase 6 — Persist LLM provider configuration per session
Goal: Store and restore LLM provider+model per session. When resuming, use stored config; when creating a session, set from current selection or CLI.

Changes:
- Define a serializable config in code_assistant for session persistence:
  - pub struct LlmSessionConfig { pub provider: llm::factory::LLMProviderType, pub model: Option<String>, pub base_url: Option<String>, pub aicore_config: Option<PathBuf>, pub num_ctx: usize, pub record_path: Option<PathBuf> }
  - Derive Serialize/Deserialize for persistence module usage
  - Note: playback and fast_playback are runtime toggles; do not persist by default

- Extend SessionState and persistence types to include llm_config: Option<LlmSessionConfig>
  - On load: if present, use it when creating the client; else fall back to CLI defaults
  - On creating a new session (terminal or GPUI), set llm_config from current selection/CLI

- Update SessionManager:
  - When starting agent for message, accept an optional LlmSessionConfig to use for client creation; or store in session and read when starting/resuming

- Update GPUI backend:
  - Use stored llm_config for existing sessions; for new sessions, initialize from provided cfg

Validations:
- search_files for SessionState struct; update serde schema and migrations (if needed)
- Ensure older sessions without llm_config continue working (Option fields default to None)
- cargo check
- cargo test


## Phase 7 — Terminal session management scaffolding
Goal: Prepare terminal mode to list/switch/create sessions, with minimal impact now and room for concurrency.

Changes:
- Extend CLI with subcommands or flags for session control (optional immediate implementation):
  - Example Subcommands: session list | session switch <id> | session new [--name <name>] [--provider ...] [--model ...]
- Alternatively/additionally: support interactive terminal commands like ":sessions", ":switch <id>", ":new"
- Ensure run_agent_terminal can connect to latest or specific session and display messages
- For future concurrency: plan to multiplex terminal input to a specific session; for now, keep a single active session at a time

Validations:
- cargo check
- Manual flow: create; list; switch; continue


## Phase 8 — Logging polish
Goal: Consistent, user-configurable logging via env or -v flags, and correct writers for modes.

Changes:
- logging::setup_logging(verbose_level: u8, to_stdout: bool)
  - Map verbosity count to filters: 0 -> info/warn, 1 -> debug for app crates, 2+ -> trace for app crates
  - Respect RUST_LOG when provided
- Use stderr for server mode (to keep stdout for JSON-RPC) and terminal mode; stdout only when needed for GPUI

Validations:
- Manual runs with -v, -vv; observe log levels


## Phase 9 — Clippy and tests
Goal: Stabilize and improve code quality.

Changes:
- Add a small unit test for CLI parsing in crates/code_assistant/src/cli.rs (e.g., ensure defaults parse as expected)
- Run cargo clippy and address warnings (unnecessary clones, needless Options, long functions)

Validations:
- cargo test
- cargo clippy -- -D warnings


## Helper snippets and skeletons

1) content.rs helper
- pub fn content_blocks_from(message: &str, attachments: &[persistence::DraftAttachment]) -> Vec<llm::ContentBlock> {
    let mut blocks = Vec::new();
    if !message.is_empty() { blocks.push(llm::ContentBlock::new_text(message.to_owned())); }
    for a in attachments {
        match a {
            persistence::DraftAttachment::Image { content, mime_type } => {
                blocks.push(llm::ContentBlock::Image { media_type: mime_type.clone(), data: content.clone() });
            }
            persistence::DraftAttachment::Text { content } => {
                blocks.push(llm::ContentBlock::new_text(content.clone()));
            }
            persistence::DraftAttachment::File { content, filename, .. } => {
                blocks.push(llm::ContentBlock::new_text(format!("File: {filename}\n{content}")));
            }
        }
    }
    blocks
}

2) llm::factory API sketch
- pub enum LLMProviderType { AiCore, Anthropic, Groq, MistralAI, Ollama, OpenAI, OpenRouter, Vertex }
- pub struct LLMClientConfig { provider: LLMProviderType, model: Option<String>, base_url: Option<String>, aicore_config: Option<PathBuf>, num_ctx: usize, record_path: Option<PathBuf>, playback_path: Option<PathBuf>, fast_playback: bool }
- pub async fn create_llm_client(cfg: LLMClientConfig) -> anyhow::Result<Box<dyn LLMProvider>> { /* move logic from main.rs */ }

3) Thin main.rs sketch
- #[tokio::main]
  async fn main() -> Result<()> {
    let args = cli::Args::parse();
    match args.mode {
      Some(cli::Mode::Server { verbose }) => app::server::run(verbose).await,
      None => {
        logging::setup_logging(args.verbose > 0, true);
        if args.ui { app::gpui::run(args)?; Ok(()) } else { app::terminal::run(args).await }
      }
    }
  }


## Acceptance criteria
- main.rs contains only argument parsing and top-level dispatch
- LLM provider creation resides in llm::factory and is reused across terminal and GPUI flows
- GPUI backend event handling lives in ui/gpui/backend.rs, with duplicated logic moved into helpers
- CLI defaults are direct (non-Option) where defaults exist; build compiles and runs
- Session persistence stores and restores the selected provider configuration; continuing a session uses the same provider
- Logging can be controlled via -v and RUST_LOG; server logs to stderr


## Pitfalls and mitigations
- Interface changes (types moved/renamed) can break imports: use search_files to find all references and update
- Avoid holding MutexGuard across await points in backend handlers; copy data needed, drop lock, then await
- Ensure old session files load by keeping new fields optional and defaulting appropriately
- Keep recorder/playback behaviors intact; treat playback as a first-class fast path in the factory


## Post-refactor follow-ups (optional)
- Decorator-based recorder to unify "new_with_recorder" variants
- GPUI UI to choose provider/model when creating a new session; persist selection
- Terminal interactive commands for session management
- Config file support (e.g., config.toml) merging with CLI/env
