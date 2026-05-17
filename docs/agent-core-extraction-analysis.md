# Agent Core Extraction — Analyse & Vision

> Status: Analyse, kein Code. Ziel ist ein wiederverwendbarer Agent-Kern (vergleichbar mit dem
> Claude Code Agent SDK), den der `code-assistant` als einen von mehreren Konsumenten nutzt.

## 1. Vision

Aus dem heutigen Crate `code_assistant` soll ein generischer Agent-Kern als eigenständiges
Crate (oder kleine Crate-Familie) extrahiert werden. Andere Anwendungen können diesen Kern
einbinden und über klar definierte Erweiterungspunkte:

- **eigene Tools** registrieren,
- **eigene Tool-Aufrufformate** (Native, XML, Caret, eigene) als Plugin einbringen,
- **eigene Verhaltens-Plugins** an festgelegten Punkten der Agent-Schleife einklinken
  (System-Prompt, Pre-/Post-LLM, Pre-/Post-Tool, Spezial-Tools, Compaction, …),
- **eigene UI-/Persistenz-/Permission-Adapter** anbinden,

ohne den Kern verändern zu müssen. Der `code-assistant` selbst wird zu einer „Reference
Implementation": er konfiguriert den Kern mit seinen Tools, Plugins und Adaptern. Das Ziel-Bild
in Crate-Form sieht so aus:

```
agent_core            (neu, generisch)         <-- Agent-Schleife, Traits, Hooks
agent_tools_core      (neu, generisch)         <-- Tool-Trait, Registry, Render, Spec
agent_persistence     (neu, generisch)         <-- SessionState/Tree-Traits (optional)
llm                   (bereits generisch)
command_executor      (bereits generisch)
fs_explorer           (bereits generisch)
sandbox               (bereits generisch)

code_assistant        (existiert)              <-- konkrete Tools, Plugins, UI, CLI
└── tool_dialects/    (XML, Caret, Native — code-assistant-intern)
```

Andere Anwendungen können sich wahlweise nur an `agent_core` + `agent_tools_core` binden, oder
zusätzlich `agent_persistence` mitnehmen.

> **Wichtig: Tool-Aufrufformate (XML / Caret / Native) gehören NICHT in den Kern.**
> Tools selbst sind heute schon syntax-agnostisch und das soll so bleiben. Was an einem
> bestimmten Format hängt, ist allein die Übersetzung zwischen LLM-Response/Stream und
> abstrakten `ToolRequest`s sowie die Darstellung der Tools im System-Prompt. Das ist ein
> reines Implementierungsdetail des Konsumenten. Der Kern definiert dafür nur ein
> minimales Trait (siehe §3.7), die konkreten XML-/Caret-/Native-Implementierungen
> bleiben Bestandteil des `code_assistant`.

---

## 2. Status quo: Wo sitzt heute die Kopplung?

Die folgende Aufstellung markiert die konkreten Stellen im `code-assistant`, die heute
Anwendungs-spezifische Annahmen über den Agent treffen. Das ist die Liste der Punkte, die
beim Refactoring entweder „nach oben" (in den Kern) oder „nach unten" (in den Konsumenten)
verschoben werden müssen.

### 2.1 `Agent` Runner (`crates/code_assistant/src/agent/runner.rs`)

`Agent::new` erwartet ein `AgentComponents`-Bündel mit lauter konkreten Typen:

- `Box<dyn LLMProvider>` — generisch, kein Problem.
- `Box<dyn ProjectManager>` — code-assistant-spezifisch (mehrere Projekte, file trees).
- `Box<dyn CommandExecutor>` — generisch, kein Problem.
- `Arc<dyn UserInterface>` — UI-Trait enthält viele code-assistant-spezifische Events.
- `Box<dyn AgentStatePersistence>` — Trait existiert, aber `SessionState` ist konkret.
- `Option<Arc<dyn PermissionMediator>>` — generisch genug.
- `Option<Arc<dyn SubAgentRunner>>` — Sub-Agent-Konzept und dessen UI-Adapter sind
  code-assistant-spezifisch.

Der Agent selbst hält darüber hinaus zustandsbehaftete code-assistant-Konzepte:

- `plan: PlanState` — Plan-Tool ist eine Funktionalität von `update_plan`, kein
  generischer Agent-Mechanismus.
- `tool_scope: ToolScope` mit Varianten `Agent`, `AgentWithDiffBlocks`, `SubAgent…` —
  bündelt mehrere Konzepte (Sub-Agent, Diff-Blocks).
- `message_nodes / active_path / next_node_id` — Branching-Modell mit
  `crate::persistence::MessageNode`.
- `tool_executions: Vec<ToolExecution>` — generisch, aber an `AnyOutput` gebunden, das
  wiederum die globale `ToolRegistry` zur (De-)Serialisierung referenziert.
- `cached_system_prompts`, `model_hint`, `session_model_config`, `context_limit_override`
  — System-Prompt-Auswahl pro Modell, Compaction-Threshold, etc.
- `file_trees`, `available_projects` — Multi-Projekt-Konzept.
- `enable_naming_reminders`, `session_name`, `pending_message_ref` — Session-Level-Logik
  die im Kern nicht generisch sein muss.

In der Loop-Implementierung selbst gibt es eine ganze Reihe **harter Spezial-Pfade**:

- `if tool_requests.iter().any(|r| r.name == "complete_task")` → Loop bricht ab.
- `if tool_request.name == "name_session"` → Title aus Input setzen, Tool als „hidden"
  behandeln, `tool_executions` direkt aktualisieren, kein UI-Update.
- `if tool_request.name == "update_plan" && success` → `save_plan_snapshot_to_last_…`.
- `can_run_in_parallel` → hartcodiert auf `spawn_agent` mit `mode=read_only`.
- `execute_spawn_agent_parallel` → eigene Code-Pfad mit konkretem `SpawnAgentTool`,
  `SpawnAgentInput`, eigenem `ToolContext`-Build, eigener `ToolStatus`-Behandlung.
- `inject_naming_reminder_if_needed` → fügt System-Reminder zum letzten User-Message,
  weil das Konzept `session_name` im Kern hängt.
- `is_prompt_too_long_error` und `replace_large_tool_results` → Pattern-Matching auf
  Anbieter-Fehlertexte und ein Fallback-Mechanismus, der `PromptTooLongError` als
  Tool-Output einsetzt; dieser Mechanismus ist generisch, aber die Kandidaten-Auswahl
  („nur das letzte User-Message-Turn-Set") ist Policy.
- `is_retryable_streaming_error` → Heuristiken auf Fehler-Strings. Generisch.
- `should_trigger_compaction` / `perform_compaction` → harter `CONTEXT_USAGE_THRESHOLD`,
  hartcodierter Compaction-Prompt aus `resources/compaction_prompt.md`.
- `read_guidance_files` (`AGENTS.md`, `CLAUDE.md`, `~/.config/code-assistant/AGENTS.md`)
  — code-assistant-spezifisches Verhalten.
- System-Prompt-Aufbau: `generate_system_message(...)` + Multi-Projekt-File-Trees +
  AGENTS.md-Guidance — das ist eine konkrete Prompt-Konstruktion.

### 2.2 `ToolContext` (`crates/code_assistant/src/tools/core/tool.rs`)

```rust
pub struct ToolContext<'a> {
    pub project_manager: &'a dyn crate::config::ProjectManager,
    pub command_executor: &'a dyn CommandExecutor,
    pub plan: Option<&'a mut PlanState>,
    pub ui: Option<&'a dyn crate::ui::UserInterface>,
    pub tool_id: Option<String>,
    pub permission_handler: Option<&'a dyn PermissionMediator>,
    pub sub_agent_runner: Option<&'a dyn crate::agent::SubAgentRunner>,
}
```

Heute ist der Tool-Kontext ein „Schweizer Taschenmesser" mit allen Subsystemen, die der
`code-assistant` braucht. Für ein generisches Crate ist das zu konkret: Tools von
Drittanwendungen brauchen weder `ProjectManager` noch `PlanState` noch `SubAgentRunner`.

### 2.3 `ToolRegistry` (`crates/code_assistant/src/tools/core/registry.rs`)

- `ToolRegistry::global()` ist ein Prozess-Singleton.
- `register_default_tools()` registriert hartcodiert alle 18 code-assistant-Tools.
- `is_tool_in_scope` filtert anhand der `ToolScope`-Enum, deren Varianten anwendungs-
  spezifisch sind.

### 2.4 `ToolSpec` / `ToolScope` (`crates/code_assistant/src/tools/core/spec.rs`)

```rust
pub enum ToolScope {
    McpServer,
    Agent,
    AgentWithDiffBlocks,
    SubAgentReadOnly,
    SubAgentDefault,
}
```

Diese Aufzählung mischt mehrere orthogonale Begriffe (MCP-Server-Modus, Sub-Agent-Modus,
Diff-Blocks-Variante) zu einer einzigen Enum. Generisch wären Tags / Capabilities besser.

### 2.5 Tool-Filter (`crates/code_assistant/src/tools/tool_use_filter.rs`)

- `is_explore_tool`, `is_write_tool`, `SmartToolFilter::is_read_tool` enthalten
  hartcodierte Listen von Tool-Namen aus dem `code-assistant`.
- Trait `ToolUseFilter` selbst ist sauber.

### 2.6 Parser / Syntax (`crates/code_assistant/src/tools/parser_registry.rs`,
`tools/parse.rs`, `tools/formatter.rs`, `tools/system_message.rs`)

- Tools selbst sind heute schon dialektfrei — sehr gut. Was an einer konkreten
  Tool-Syntax hängt, ist nur die Übersetzung zwischen LLM-Stream/Response und
  abstrakten `ToolRequest`s plus die Darstellung im System-Prompt.
- Trait `ToolInvocationParser` ist sauber, registriert wird aber über die fixe Enum
  `ToolSyntax { Native, Xml, Caret }`. Eine Drittanwendung kann kein eigenes Format
  nachladen, ohne den Kern zu patchen.
- Der Parser greift in mehreren Stellen direkt auf `ToolRegistry::global()` zu
  (Schema-getriebene Konvertierung).
- `is_multiline_param` enthält eine hartcodierte Allow-Liste konkreter Parameter-Namen
  (`content`, `command_line`, `diff`, `message`, `old_text`, `new_text`).
- XML-/Caret-Doku-Generatoren erfinden Beispiel-Werte basierend auf Parameter-Namen
  (`project`, `path`, `regex`, `command_line`, `working_dir`, `url`, …) — also ebenfalls
  Code-Assistant-Vokabular.
- `system_message::generate_system_message` lädt eingebettete Markdown-Templates und
  Modell-Mapping aus `resources/`.

Im Zielbild verschwindet die ganze Datei-Gruppe aus dem Agent-Kern und wird ein
internes Modul des `code_assistant` (siehe §3.7).

### 2.7 UI-Trait (`crates/code_assistant/src/ui/mod.rs`,
`crates/code_assistant/src/ui/ui_events.rs`)

Das `UserInterface`-Trait selbst ist klein, aber `UiEvent` ist riesig und enthält stark
anwendungs-spezifische Varianten:

- `UpdateSessionMetadata`, `UpdateSessionActivityState`, `RefreshChatList`,
  `UpdateChatList`, `BranchSwitched`, `StartMessageEdit`, `MessageEditReady`,
  `UpdateBranchInfo`, `UpdateWorktreeData`, `UpdateSandboxPolicy`, `CancelSubAgent`,
  `PersistUiState`, `RefreshCurrentSession`, `AppendMessages`, `ResourceLoaded`,
  `ResourceWritten`, `DirectoryListed`, `ResourceDeleted`, `UpdatePlan`, …

Die wirklich „Agent-Kern"-relevanten Events sind dagegen klein:
`StreamingStarted/Stopped`, `RollbackStreaming`, `UpdateToolStatus`, `UpdateToolParameter`,
`AppendToTextBlock`, `AppendToThinkingBlock`, `StartTool`, `EndTool`, …

### 2.8 Persistence (`crates/code_assistant/src/persistence.rs`,
`crates/code_assistant/src/agent/persistence.rs`)

- `SessionState` enthält `message_nodes`, `active_path`, `tool_executions`, `plan`,
  `config: SessionConfig`, `next_request_id`, `model_config: SessionModelConfig`. Die
  Branch-Tree-Struktur ist konzeptuell allgemein, die Felder `plan` und `model_config`
  jedoch nicht.
- `AgentStatePersistence::save_agent_state(state: SessionState)` koppelt also den Trait
  hart an die konkrete Struktur.
- `SerializedToolExecution::deserialize` greift wieder auf `ToolRegistry::global()` zu.

### 2.9 Sub-Agents (`crates/code_assistant/src/agent/sub_agent.rs`)

- `SubAgentRunner` als Trait ist okay, der Default-Runner mischt jedoch sehr viele
  konkrete Aspekte (Sandbox, `DefaultProjectManager`, `SessionConfig`, eigene UI-Adapter,
  `SubAgentToolCall`/`SubAgentOutput`-JSON-Form für Custom-Renderer).
- Der `spawn_agent`-Tool-Output ist eng mit der UI-JSON-Form verknüpft.

### 2.10 Special-Tools mit fester Bedeutung im Loop

Folgende Tool-Namen sind *strings, die im Agent-Loop hartverdrahtet sind*:

| Tool             | Wo verdrahtet                                  | Effekt |
|------------------|-----------------------------------------------|--------|
| `complete_task`  | `manage_tool_execution`                        | bricht Schleife ab |
| `name_session`   | `execute_tool` (vor Standard-Pfad)             | setzt `session_name`, kein UI-Update |
| `update_plan`    | nach erfolgreichem `execute_tool`              | speichert Plan-Snapshot in MessageNode |
| `spawn_agent`    | `can_run_in_parallel`, `execute_spawn_agent_parallel` | erlaubt parallele Ausführung & Spezial-UI |
| `parse_error`    | `agent::types`, `persistence`                  | Pseudo-Tool für Parse-Fehler |

Plus implizit über den `SmartToolFilter`: `read_files`, `list_files`, `list_projects`,
`search_files`, `glob_files`, `web_fetch`, `web_search` (read), `write_file`,
`replace_in_file`, `delete_files` (write), `execute_command` (write).

### 2.11 Resources / Templates

- `resources/compaction_prompt.md` — fester Compaction-Prompt
- `resources/tool_use_intro.md` — Einleitung der System-Prompt-Tool-Beschreibung
- `resources/system_prompts/{default, claude, codex}.md` + `mapping.json` — modell-
  spezifische Basis-Prompts mit `{{syntax}}` und `{{tools}}` Platzhaltern.

---

## 3. Ziel-Architektur

### 3.1 Crate-Aufteilung

```
agent_core
├── lib.rs                  (re-exports)
├── agent/
│   ├── runtime.rs          (AgentRuntime, AgentLoop)
│   ├── config.rs           (AgentConfig — non-session)
│   ├── flow.rs             (LoopFlow, IterationOutcome)
│   └── error.rs
├── messages/
│   └── tree.rs             (MessageTree, NodeId — optional feature)
├── hooks/                  (siehe §3.5)
│   ├── prompt.rs
│   ├── lifecycle.rs
│   ├── tool_dispatch.rs
│   ├── compaction.rs
│   └── retry.rs
├── persistence.rs          (StatePersistence-Trait, AgentSnapshot)
├── ui.rs                   (AgentUi-Trait, AgentUiEvent — Minimal-Set)
└── permissions.rs          (PermissionMediator-Trait)

agent_tools_core
├── lib.rs
├── tool.rs                 (Tool-Trait, ToolContext mit Extensions)
├── dyn_tool.rs             (DynTool, AnyOutput)
├── registry.rs             (ToolRegistry — Instanz statt Singleton)
├── spec.rs                 (ToolSpec, Capability-Tags)
├── render.rs               (Render, ResourcesTracker, ImageData)
├── result.rs               (ToolResult, ToolError)
└── title.rs                (Title-Templating)
   # Im Kern: nur ein abstraktes ToolDialect-Trait (siehe §3.7).
   # Konkrete XML-/Caret-/Native-Implementierungen leben im code_assistant.

code_assistant            (existiert, nutzt obige Crates)
├── tools/                  (impls, registriert in eigener Registry-Instanz)
├── tool_dialects/          (XML/Caret/Native: Parser, Formatter, StreamProcessor,
│                            System-Prompt-Tool-Doku — alles intern)
├── plugins/                (NEU: code-assistant-spezifische Hooks)
│   ├── plan.rs             (Plan-Tool-Hook)
│   ├── name_session.rs     (Name-Reminder + Spezial-Tool)
│   ├── projects.rs         (File-Trees + AGENTS.md im System-Prompt)
│   ├── compaction.rs       (Threshold + Prompt)
│   ├── prompt_too_long.rs  (Recovery-Strategie)
│   └── sub_agent.rs        (Sub-Agent-Plugin)
├── ui/                     (alle bisherigen UI-Events bleiben hier)
├── session/                (Branching, SessionInstance, Manager)
└── ...
```

Das genaue Aufteilen kann auch in einer einzigen Crate `agent_core` mit Sub-Modulen
beginnen und später in mehrere Crates gesplittet werden.

### 3.2 Generischer `Agent`-Kern

Der zentrale Typ wird vom Anwendungs-Zustand entkoppelt:

```rust
// agent_core::agent::runtime
pub struct AgentRuntime<E: AgentExtensions> {
    llm: Box<dyn LLMProvider>,
    tools: Arc<ToolRegistry<E::ToolExt>>,
    parser: Arc<dyn ToolInvocationParser>,
    formatter: Arc<dyn ToolFormatter>,
    ui: Arc<dyn AgentUi>,
    state: Box<dyn StatePersistence<Snapshot = E::Snapshot>>,
    permissions: Option<Arc<dyn PermissionMediator>>,
    hooks: HookRegistry<E>,
    config: AgentConfig,
    session: SessionContext,             // generischer, kleiner Container
    extensions: E::State,                // app-spezifischer Zustand
}
```

`AgentExtensions` ist ein vom Konsumenten implementiertes Trait, das alle Variations-
Punkte zusammenfasst:

```rust
pub trait AgentExtensions: Send + Sync + 'static {
    /// Anwendungs-spezifischer Zustand, den Hooks lesen/schreiben dürfen.
    type State: Send + Sync;

    /// Anwendungs-spezifischer Tool-Kontext-Slice (siehe §3.4).
    type ToolExt: ToolContextExtension;

    /// Persistierter Zustand (heutige `SessionState`-Felder, soweit benötigt).
    type Snapshot: Serialize + DeserializeOwned + Send + Sync;
}
```

### 3.3 Generischer Agent-Loop

Heute werkelt `Agent::run_single_iteration` mit ~100 Zeilen Spezialfällen. Das Ziel ist,
diese Schleife auf ein simples, deterministisches Skelett zu reduzieren, an dem Plugins
einklinken. Pseudocode:

```rust
loop {
    hooks.before_iteration(ctx).await?;

    // 1. Pending-User-Message ggf. anhängen
    hooks.collect_pending_user_input(ctx).await?;

    // 2. Compaction-Politik fragen
    if hooks.compaction_policy(ctx)?.should_compact() {
        hooks.run_compaction(ctx).await?;
        continue;
    }

    // 3. Render-Phase: TooLResults dynamisch ersetzen, Reminder injizieren ...
    let mut request = ctx.build_llm_request();
    hooks.shape_request(&mut request, ctx).await?;

    // 4. LLM-Call (mit Retry-Hook)
    let response = ctx.send_request(request, hooks.retry_policy()).await?;
    hooks.observe_response(&response, ctx).await?;

    // 5. Tool-Extraktion (delegiert an Parser)
    let (tool_requests, flow) = hooks.extract_tools(&response, ctx)?;

    // 6. Spezial-Tool-Dispatch
    if let Some(decision) = hooks.intercept_tools(&tool_requests, ctx).await? {
        match decision { Break => return, GetUserInput => return, Continue => continue, ... }
    }

    // 7. Tool-Ausführung (parallel oder sequenziell, vom Hook entschieden)
    let results = hooks.execute_tools(tool_requests, ctx).await?;

    // 8. Ergebnisse zurück in den State
    hooks.record_tool_results(results, ctx).await?;

    hooks.after_iteration(ctx).await?;
}
```

Die genannten `hooks.*`-Aufrufe bilden den **stabilen Vertrag** zwischen Kern und
Konsument. Defaults im Kern verhalten sich „neutral" (kein Spezial-Verhalten), so dass
ein minimaler Konsument keinerlei Plugin schreiben muss.

### 3.4 Generischer `ToolContext`

Statt eines monolithischen Structs wird der Tool-Kontext ein „Service Locator" mit
typsicheren Extensions:

```rust
// agent_tools_core::tool
pub struct ToolContext<'a, Ext: ToolContextExtension = ()> {
    pub command_executor: &'a dyn CommandExecutor,
    pub ui: Option<&'a dyn AgentUi>,
    pub tool_id: Option<&'a str>,
    pub permissions: Option<&'a dyn PermissionMediator>,
    pub cancel: &'a CancellationToken,
    pub ext: &'a mut Ext,                 // anwendungs-spezifisch
}

pub trait ToolContextExtension: Send {
    /// Optional: Lookup spezifischer Sub-Services nach TypeId.
    fn get<T: 'static>(&self) -> Option<&T> { None }
    fn get_mut<T: 'static>(&mut self) -> Option<&mut T> { None }
}
```

Der `code-assistant` definiert dann einmalig:

```rust
struct CaExt {
    project_manager: Box<dyn ProjectManager>,
    plan: Option<PlanState>,
    sub_agent_runner: Option<Arc<dyn SubAgentRunner>>,
    session_id: Option<String>,
    // ...
}
impl ToolContextExtension for CaExt { ... }
```

und seine Tools erwarten `ToolContext<'_, CaExt>`. Tools fremder Anwendungen sehen ihre
eigene Extension. `CommandExecutor` und `PermissionMediator` bleiben im Kern, weil sie
schon generisch sind.

Alternativ-Design: ein heterogener Service-Locator über `AnyMap`/`TypeMap`. Der typisierte
Ansatz ist sicherer, der `AnyMap`-Ansatz öffnet die Plugin-Architektur stärker.

### 3.5 Hooks / Plugins (das Herzstück)

Die wichtigsten Erweiterungspunkte als kleine Traits, die die Default-Implementation
„passthrough" macht:

```rust
/// Greift in den Aufbau des System-Prompts ein.
#[async_trait]
pub trait SystemPromptProvider: Send + Sync {
    async fn build(
        &self,
        ctx: &PromptContext<'_>,
    ) -> Result<String>;
}

/// Pre-/Post-Hooks der Iteration (Logging, Reminder injizieren, ...).
#[async_trait]
pub trait IterationHook: Send + Sync {
    async fn before_iteration(&self, ctx: &mut LoopCtx<'_>) -> Result<()> { Ok(()) }
    async fn shape_request(
        &self,
        request: &mut LLMRequest,
        ctx: &mut LoopCtx<'_>,
    ) -> Result<()> { Ok(()) }
    async fn observe_response(
        &self,
        response: &LLMResponse,
        ctx: &mut LoopCtx<'_>,
    ) -> Result<()> { Ok(()) }
    async fn after_iteration(&self, ctx: &mut LoopCtx<'_>) -> Result<()> { Ok(()) }
}

/// Spezielle Tool-Namen abfangen (complete_task, name_session, …).
#[async_trait]
pub trait ToolInterceptor: Send + Sync {
    /// Wird vor der Standard-Ausführung aufgerufen.
    /// Rückgabe `Some(_)` ersetzt die Standard-Ausführung.
    async fn try_handle(
        &self,
        tool: &ToolRequest,
        ctx: &mut LoopCtx<'_>,
    ) -> Result<Option<InterceptOutcome>>;
}

pub enum InterceptOutcome {
    /// Tool wurde abgehandelt; ToolResult ist optional.
    Handled { result: Option<Box<dyn AnyOutput>>, hidden_in_ui: bool },
    /// Tool soll Loop beenden.
    BreakLoop,
    /// Tool soll Loop pausieren und auf User warten.
    AwaitUser,
}

/// Strategie für parallele Ausführung (heute hartkodiert auf spawn_agent).
pub trait ToolDispatchPolicy: Send + Sync {
    fn partition<'r>(&self, requests: &'r [ToolRequest]) -> ToolBatchPlan<'r>;
}

/// Compaction-Policy.
pub trait CompactionPolicy: Send + Sync {
    fn should_compact(&self, snapshot: &ContextSnapshot) -> bool;
    fn compaction_prompt(&self) -> &str;
}

/// Retry-/Recovery-Politik (PromptTooLong, Streaming-Errors, ...).
pub trait RecoveryPolicy: Send + Sync {
    fn classify(&self, err: &anyhow::Error) -> RecoveryAction;
}

pub enum RecoveryAction {
    Fail,                      // Fehler weiterreichen
    RetryStream { delay: Duration },
    ReduceContext,             // delegiert an `ContextReducer`
}

pub trait ContextReducer: Send + Sync {
    fn try_reduce(&self, ctx: &mut LoopCtx<'_>) -> Result<bool>;
}

/// Filter, der bestimmt, welche Tool-Folgen im Stream erlaubt sind.
pub trait ToolUseFilter: Send + Sync { ... }   // Trait existiert bereits

/// Persistenter Zustand abstrahiert.
pub trait StatePersistence: Send + Sync {
    type Snapshot: Send + Sync;
    fn save(&mut self, snapshot: &Self::Snapshot) -> Result<()>;
    fn load(&self, id: &str) -> Result<Option<Self::Snapshot>>;
}
```

Hooks werden in einer `HookRegistry<E>` zusammengefasst und am Anfang der `AgentRuntime`-
Konstruktion vom Konsumenten gesetzt, z. B. via Builder:

```rust
let runtime = AgentRuntimeBuilder::<CaExt>::new(llm, ui)
    .with_tools(my_tool_registry)
    .with_parser(parser_for(ToolSyntax::Caret))
    .with_system_prompt(Box::new(CodeAssistantSystemPrompt::new(...)))
    .add_iteration_hook(Box::new(NameSessionReminderHook))
    .add_iteration_hook(Box::new(ProjectInfoHook))
    .add_tool_interceptor(Box::new(NameSessionInterceptor))
    .add_tool_interceptor(Box::new(CompleteTaskInterceptor))
    .add_tool_interceptor(Box::new(UpdatePlanSnapshotInterceptor))
    .with_dispatch_policy(Box::new(SpawnAgentParallelPolicy))
    .with_compaction(Box::new(TokenRatioCompaction { threshold: 0.8, prompt: ... }))
    .with_recovery(Box::new(DefaultRecovery))
    .with_context_reducer(Box::new(DropLargestToolResults))
    .with_state_persistence(state_persistence)
    .build();
```

### 3.6 Generischer `ToolRegistry` und `ToolSpec`

Statt eines globalen Singletons wird `ToolRegistry<Ext>` instantiierbar und Generic über
die Tool-Context-Extension:

```rust
pub struct ToolRegistry<Ext: ToolContextExtension> {
    tools: HashMap<String, Box<dyn DynTool<Ext>>>,
}
```

`ToolScope` wird zu freien **Capability-Tags**:

```rust
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters_schema: serde_json::Value,
    pub annotations: Option<serde_json::Value>,
    pub capabilities: &'static [&'static str], // z. B. "read_only", "edits_files"
    pub hidden: bool,
    pub title_template: Option<&'static str>,
}
```

Selektion erfolgt über frei kombinierbare Filter-Funktionen, z. B.
`registry.iter().filter(|t| t.has_capability("read_only"))`. Damit verschwindet die
hartkodierte `ToolScope`-Enum aus dem Kern.

`is_explore_tool` / `is_write_tool` / `SmartToolFilter` werden zu reinen Konsumenten-
Helfer, die die Capability-Tags der jeweiligen Tools auswerten — ohne Tool-Namen zu kennen.

### 3.7 Tool-Aufrufformat als Plugin (kein Kern-Bestandteil)

Der Agent-Kern darf **keine** XML-, Caret- oder Native-Spezifik enthalten. Er kennt nur
abstrakte `ToolRequest`s, abstrakte LLM-Antworten und einen abstrakten Stream-Prozessor
für die UI. Die Übersetzung zwischen einem konkreten Aufrufformat („Tool-Dialekt") und
diesen abstrakten Begriffen wird über *ein* kleines Plugin-Trait gekapselt:

```rust
// agent_core::tool_dialect

/// Wie ein Tool-Aufruf zwischen LLM und Agent reist.
/// Implementierungen leben im Konsumenten (z. B. `code_assistant::tool_dialects::xml`).
pub trait ToolDialect: Send + Sync {
    /// Aus einer fertigen LLM-Antwort `ToolRequest`s extrahieren.
    /// Liefert zusätzlich eine ggf. an der ersten Tool-Stelle abgeschnittene Variante
    /// der Response zurück, damit Folge-Text nach einem Tool-Block nicht ins Transkript
    /// gelangt.
    fn extract_requests(
        &self,
        response: &LLMResponse,
        request_id: u64,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)>;

    /// Ein Stream-Prozessor, der `StreamingChunk`s in `DisplayFragment`s übersetzt.
    fn stream_processor(
        &self,
        ui: Arc<dyn AgentUi>,
        request_id: u64,
    ) -> Box<dyn StreamProcessor>;

    /// Wie der Dialekt die LLM-Tool-Liste in den `LLMRequest` einspeist:
    /// - Native: `Some(tool_definitions)` — die LLM-API kennt die Tools nativ.
    /// - XML / Caret: `None` — die Tools werden im System-Prompt beschrieben.
    fn populate_request_tools(
        &self,
        tools: &ToolRegistry<impl ToolContextExtension>,
        capabilities: ToolCapabilityFilter,
    ) -> Option<Vec<ToolDefinition>>;

    /// Optional: Block für die Tool-Doku im System-Prompt. `None` für Native.
    fn render_tool_section_for_prompt(
        &self,
        tools: &ToolRegistry<impl ToolContextExtension>,
        capabilities: ToolCapabilityFilter,
    ) -> Option<String>;

    /// Optional: Format-Beschreibung („So rufst du Tools auf …"). `None` für Native.
    fn render_format_section_for_prompt(&self) -> Option<String>;

    /// Erkennt, ob eine bereits gespeicherte Nachricht einen Tool-Aufruf in *diesem*
    /// Dialekt enthält (für Normalisierung beim Laden des Verlaufs).
    fn message_contains_invocation(&self, message: &Message) -> bool;
}
```

Damit liegt die ganze heutige `parser_registry` / `formatter` / `stream_processor` /
`system_message`-Maschinerie im Konsumenten. Der `code_assistant` liefert seine drei
Dialekte (`XmlDialect`, `CaretDialect`, `NativeDialect`) als interne Implementierungen
und wählt zur Laufzeit anhand der Session-Konfiguration eine aus.

Was der Kern stattdessen liefert:

- Trait `ToolDialect` und Trait `StreamProcessor` (klein, syntaxneutral).
- Der `AgentRuntimeBuilder` erwartet beim Bauen einen `Box<dyn ToolDialect>`.
- Der `SystemPromptProvider` (siehe §3.5) bekommt den Dialekt durchgereicht und kann
  ihn nach `render_format_section_for_prompt` und `render_tool_section_for_prompt`
  fragen, wenn er den System-Prompt aufbaut.

Damit gilt:

- **Keine `ToolSyntax`-Enum** mehr im Kern. Welche Dialekte existieren, weiß nur der
  Konsument.
- **Keine globale `ParserRegistry`** mehr. Es gibt einfach immer genau einen Dialekt,
  der pro Agent-Instanz gesetzt wird.
- **Kein `agent_tools_syntax`-Crate.** Die heutigen XML-/Caret-/Native-Implementierungen
  wandern als interne Module in den `code_assistant` (z. B. unter `tool_dialects/`).
  Wenn jemand sie wirklich teilen will, kann das später ein optionales Helfer-Crate
  werden — aber das ist ausdrücklich kein Pflichtteil.
- **Tools selbst bleiben dialektfrei.** Sie kennen weiterhin nur ihr JSON-Schema und
  ihren `Render`-Output.

Die Folgen für ein paar konkrete heutige Aufräumarbeiten:

- `is_multiline_param` (heute hartcodierte Allow-Liste) ist ein Detail des XML-/Caret-
  Dialekts, nicht des Kerns. Es darf weiterhin im `code_assistant` leben — und dort
  am besten datengetrieben (z. B. ein `multiline: true` im JSON-Schema-Feld oder über
  eine Helper-Methode am `Tool`-Trait wie `multiline_params() -> &'static [&'static str]`,
  damit die Liste neben dem Tool steht und nicht zentral.).
- Die XML-/Caret-Doku-Generatoren mit ihren „magic placeholder names" (`project`, `path`,
  `regex`, …) sind ebenfalls reines Dialekt-Detail. Mittelfristig sollten sie ihre
  Beispiel-Werte aus `examples` im JSON-Schema beziehen, damit die Liste nicht mehr
  Tool-Namen kennt — aber auch das passiert im `code_assistant`, nicht im Kern.
- `SerializedToolExecution::deserialize` greift heute auf `ToolRegistry::global()` zu;
  beim Crate-Split bekommt es die `ToolRegistry<Ext>` als Argument (siehe §3.10). Das
  ist unabhängig vom Dialekt-Thema.

### 3.8 Generisches UI-Event-Set

Im Kern existiert nur ein **kleines, agent-zentrisches** Event-Set:

```rust
pub enum AgentUiEvent {
    StreamingStarted { request_id: u64, thread_node_id: Option<u64> },
    StreamingStopped { request_id: u64, cancelled: bool, error: Option<String> },
    RollbackStreaming { request_id: u64 },

    StartTool { tool_id: String, name: String },
    UpdateToolParameter { tool_id: String, name: String, value: String, replace: bool },
    UpdateToolStatus { tool_id: String, status: ToolStatus, message: Option<String>, output: Option<String>, ... },
    EndTool { tool_id: String },
    ToolOutputChunk { tool_id: String, chunk: String },

    AppendText(String),
    AppendThinking(String),
    AddImage { media_type: String, data: String },

    ReasoningSummaryStart,
    ReasoningSummaryDelta(String),
    ReasoningComplete,

    ShowTransientStatus(String),
    ClearTransientStatus,

    /// Anwendungs-spezifische Events.
    Custom(Box<dyn Any + Send + Sync>),
}
```

Die heutigen `UiEvent`-Varianten zu Sessions, Branching, Worktrees, Sandbox, Drafts etc.
gehören in den `code_assistant`-Layer und reisen über `AgentUiEvent::Custom`. Damit ist
das `UserInterface`-Trait für Drittanwendungen klein und überschaubar.

### 3.9 Generische Persistenz

`StatePersistence` wird über den Snapshot-Typ generisch. Der Kern erlaubt einen
„Standard-Snapshot" mit MessageTree + ToolExecutions, ergänzbar mit anwendungs-
spezifischen Feldern:

```rust
pub trait StatePersistence: Send + Sync {
    type Snapshot;
    fn save(&mut self, s: &Self::Snapshot) -> Result<()>;
    fn load(&self, id: &str) -> Result<Option<Self::Snapshot>>;
}

pub struct CoreSnapshot {
    pub session_id: String,
    pub message_tree: MessageTree,        // wie heute MessageNodes/active_path
    pub tool_executions: Vec<ToolExecutionRecord>,
    pub next_request_id: u64,
    pub next_node_id: u64,
}
```

`code-assistant` setzt seinen Snapshot-Typ z. B. auf
`(CoreSnapshot, CodeAssistantSessionExt)`, und die Serialisierung passiert in einem
Wrapper-Persistence-Adapter.

`ToolExecution::deserialize` wird per Registry-Argument (statt Singleton) entkoppelt.

### 3.10 Persistenz von Tool-Outputs ohne Singleton

Heute verlässt sich `SerializedToolExecution::deserialize` auf
`ToolRegistry::global()`. In der Zielarchitektur muss der Lader die Registry kennen.
Mehrere Optionen:

1. **Registry als Argument**: `deserialize(&self, registry: &ToolRegistry<Ext>) -> ...`.
2. **Selbst-beschreibende Outputs**: `ToolOutput` enthält genug Typ-Tag, um per
   Deserializer-Map (von Output-Tag → `Box<dyn AnyOutput>`-Konstruktor) zu rekonstruieren.

Variante 1 ist am einfachsten und passt zur generellen Linie „weg vom Singleton".

---

## 4. Zuordnung der heutigen Spezial-Pfade auf Hooks

| Heute (im `Agent`)                                   | Ziel                                       |
|------------------------------------------------------|---------------------------------------------|
| `complete_task`-Spezial im `manage_tool_execution`   | `ToolInterceptor` → `BreakLoop`             |
| `name_session`-Spezial in `execute_tool`             | `ToolInterceptor::Handled { hidden_in_ui: true }` + `IterationHook::shape_request` für Reminder |
| `update_plan` → Plan-Snapshot                        | `ToolInterceptor::after_success` Variante / `IterationHook::after_iteration` |
| `spawn_agent` parallel mit `mode=read_only`          | `ToolDispatchPolicy::partition` im Plugin   |
| `inject_naming_reminder_if_needed`                   | `IterationHook::shape_request`              |
| `convert_tool_results_to_text` (XML/Caret)           | `ToolInvocationParser`-Methode oder eigener Hook |
| `render_tool_results_in_messages` (synthetic cancellations) | Kern-Funktion; bleibt generisch         |
| `is_prompt_too_long_error` + `replace_large_tool_results` | `RecoveryPolicy` + `ContextReducer`     |
| `is_retryable_streaming_error`                       | `RecoveryPolicy::classify`                  |
| `should_trigger_compaction` + `perform_compaction`   | `CompactionPolicy` + `ContextReducer::compact` |
| `read_guidance_files` (AGENTS.md/CLAUDE.md)          | `SystemPromptProvider`-Plugin               |
| `init_projects` / `file_trees` / `available_projects`| `SystemPromptProvider`-Plugin               |
| `cached_system_prompts` / Modell-Mapping             | `SystemPromptProvider`-Implementierung      |
| `tool_scope` / Diff-Blocks-Variante                  | `Capability`-Tags + Konfigurations-Flag     |
| `pending_message_ref`, `update_activity_state`,
  `build_current_metadata`, `save_state` →
  `ChatMetadata`-Update                               | bleibt im `code_assistant` (über `IterationHook::after_iteration` und ein `SessionExtension`); Kern hält nur `MessageTree`-Snapshot |
| Sub-Agent-Output-JSON-Format (`SubAgentOutput`)      | bleibt im `code_assistant` als Tool-Output  |
| Branching (`MessageNode`, `active_path`, …)          | optional als Feature `branching` im Kern    |

---

## 5. Konkrete Typ-Skizzen

> Diese Skizzen sind absichtlich grob gehalten. Sie sollen zeigen, wie sich die heutigen
> Strukturen in die generischen Bausteine übersetzen.

### 5.1 `AgentConfig`

```rust
pub struct AgentConfig {
    pub max_streaming_retries: u32,
    pub streaming_retry_base_delay: Duration,
    pub default_tool_syntax: SyntaxId,
    pub max_iterations: Option<u32>,         // Optional; None = unbegrenzt
    // KEIN model_hint, KEIN sandbox_policy, KEIN init_path, KEIN initial_project — das
    // sind code-assistant-Konzepte und gehören in dessen Extension.
}
```

### 5.2 `LoopCtx`

```rust
pub struct LoopCtx<'a, E: AgentExtensions> {
    pub messages: &'a mut MessageTree,
    pub tool_executions: &'a mut Vec<ToolExecutionRecord>,
    pub ui: &'a dyn AgentUi,
    pub llm: &'a dyn LLMProvider,
    pub parser: &'a dyn ToolInvocationParser,
    pub formatter: &'a dyn ToolFormatter,
    pub tool_registry: &'a ToolRegistry<E::ToolExt>,
    pub permissions: Option<&'a dyn PermissionMediator>,
    pub session: &'a SessionContext,
    pub extensions: &'a mut E::State,
    pub config: &'a AgentConfig,
}
```

### 5.3 Beispiel: `NameSessionInterceptor`

```rust
struct NameSessionInterceptor;

#[async_trait]
impl ToolInterceptor for NameSessionInterceptor {
    async fn try_handle(
        &self,
        tool: &ToolRequest,
        ctx: &mut LoopCtx<'_, CaExt>,
    ) -> Result<Option<InterceptOutcome>> {
        if tool.name != "name_session" { return Ok(None); }

        let title = tool.input.get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing title"))?;

        ctx.extensions.session_name = title.to_string();
        let output = NameSessionOutput { title: title.into() };

        Ok(Some(InterceptOutcome::Handled {
            result: Some(Box::new(output)),
            hidden_in_ui: true,
        }))
    }
}
```

### 5.4 Beispiel: `CodeAssistantSystemPrompt`

```rust
struct CodeAssistantSystemPrompt {
    base_prompts: PromptMapping,
    tool_intro: &'static str,
    project_manager: Arc<dyn ProjectManager>,
    cache: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl SystemPromptProvider for CodeAssistantSystemPrompt {
    async fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        // 1) Modell-spezifischen Basis-Prompt wählen
        // 2) Syntax-Doc + Tool-Doc vom Parser holen
        // 3) Projekt-File-Trees & AGENTS.md anhängen
        // ... wie heute, aber kapselt den gesamten code-assistant-Anteil.
    }
}
```

### 5.5 Beispiel: `SpawnAgentParallelPolicy`

```rust
struct SpawnAgentParallelPolicy;

impl ToolDispatchPolicy for SpawnAgentParallelPolicy {
    fn partition<'r>(&self, reqs: &'r [ToolRequest]) -> ToolBatchPlan<'r> {
        let (parallel, sequential) = reqs.iter().partition(|r| {
            r.name == "spawn_agent"
                && r.input.get("mode").and_then(|v| v.as_str()).unwrap_or("read_only") == "read_only"
        });
        ToolBatchPlan { parallel, sequential }
    }
}
```

### 5.6 Beispiel: `TokenRatioCompaction`

```rust
struct TokenRatioCompaction {
    threshold: f32,
    prompt: &'static str,
    context_limit: Box<dyn Fn(&SessionContext) -> Option<u32> + Send + Sync>,
}

impl CompactionPolicy for TokenRatioCompaction { ... }
```

---

## 6. Migrations-Plan in Phasen

Die Umstellung lässt sich in mehreren Schritten durchführen, ohne dass die Anwendung
zwischenzeitlich kaputt ist:

### Phase 1 — Hook-Punkte einführen, ohne Crate-Split

1. Spezial-Pfade aus `Agent::run_single_iteration` in private Methoden extrahieren:
   `intercept_special_tool`, `apply_naming_reminder`, `partition_parallel_tools`,
   `compaction_policy`, `recovery_policy`. Verhalten bleibt gleich.
2. Globale `ToolRegistry::global()`-Aufrufe in Parser/Persistence durch ein injizierbares
   Argument ersetzen (Compatibility-Wrapper, der weiter `global()` nutzt, bleibt vorerst).
3. `ToolContext` um eine `extensions: &mut dyn Any`-Backdoor erweitern, damit Tools, die
   schon generischer sein könnten (`read_files`, `web_search`, …), keine `ProjectManager`-
   Referenz mehr brauchen.

### Phase 2 — Plugin-Traits einführen

1. Traits `IterationHook`, `ToolInterceptor`, `ToolDispatchPolicy`, `CompactionPolicy`,
   `RecoveryPolicy`, `ContextReducer`, `SystemPromptProvider`, `StatePersistence` als
   Module unter `crates/code_assistant/src/agent/hooks/` anlegen.
2. Existierende Spezial-Pfade als Plugin-Implementierungen umziehen (`PlanSnapshotHook`,
   `NameSessionInterceptor`, `CodeAssistantSystemPrompt`, …).
3. `Agent::run_single_iteration` ruft nur noch über die Hook-Registry. Tests bleiben
   dieselben.

### Phase 3 — `ToolScope` → Capabilities, `is_multiline_param` → schema-driven

1. `ToolSpec` um `capabilities: &'static [&'static str]` erweitern (parallel zu `supported_scopes`).
2. `SmartToolFilter` und Sub-Agent-Logik auf Capabilities umstellen.
3. Multiline-Parameter und Doku-Beispiele aus dem JSON-Schema ableiten; `is_multiline_param`
   und „magic placeholder names" entfernen.
4. `ToolScope`-Enum entfernen oder zu `enum ToolScope(&'static str)` (just-a-tag) reduzieren.

### Phase 4 — Crate-Split

1. Neues Crate `agent_tools_core` anlegen, dorthin verschieben:
   `tools/core/{tool, dyn_tool, registry, render, result, spec, title}.rs`.
2. Neues Crate `agent_core`: `agent/runner.rs`, neue Hook-Module, `MessageTree`,
   `AgentUiEvent`-Minimum, `StatePersistence`-Trait, abstraktes `ToolDialect`-Trait
   und `StreamProcessor`-Trait — *aber keine konkreten XML-/Caret-/Native-
   Implementierungen*.
3. `code_assistant`:
   - Verschiebt `tools/{parse, formatter, parser_registry, system_message}.rs` und
     `ui/streaming/{xml,caret,json}_processor.rs` in ein internes Modul
     `tool_dialects/` und implementiert dort `ToolDialect` + `StreamProcessor` für
     XML, Caret und Native. Diese Implementierungen bleiben Anwendungs-intern.
   - `tool_use_filter.rs` (`SmartToolFilter`) bleibt im `code_assistant` — wertet
     nach dem Refactoring Capability-Tags statt Tool-Namen aus (vgl. Phase 3).
   - Implementiert `AgentExtensions` (`CaExt`, Snapshot, ToolExt).
   - Verschiebt `UiEvent` in eine `code_assistant_ui`-Lib oder belässt sie und packt sie
     als `AgentUiEvent::Custom`.
   - Behält Branching, Sub-Agents, Sessions, Persistence-Files.
4. Optional: `agent_persistence` mit JSON-File-Adapter herausziehen.

### Phase 5 — Aufräumen

1. `ToolRegistry::global()` entfernen; alle Aufrufer bekommen die Registry per Argument
   oder über den `ToolContext`/`LoopCtx`.
2. `ParserRegistry`-Singleton ersatzlos streichen — pro Agent existiert genau ein
   `Box<dyn ToolDialect>`, das beim Bauen gesetzt wird. Die `ToolSyntax`-Enum entfällt
   im Kern; im `code_assistant` darf sie als interner Auswahl-Helfer für die mitgelieferten
   Dialekte bestehen bleiben.
3. Sub-Agent-spezifische UI-Adapter ins `code_assistant` verschieben; im Kern bleibt
   nur `SubAgentRunner`-Trait — und auch der ist optional.
4. Resources (`compaction_prompt.md`, `tool_use_intro.md`, `system_prompts/*.md`) in
   den `code_assistant` verschieben (keine Default-Prompts im Kern).

---

## 7. Offene Fragen / Designentscheidungen

1. **Branching im Kern oder im Konsumenten?** Das `MessageTree`-Modell ist nicht
   trivial, aber auch nicht universal. Vorschlag: Im Kern hinter Feature-Flag
   `branching` anbieten, mit linearer Default-Variante.
2. **`SubAgentRunner` im Kern?** Sub-Agenten sind ein Re-Entry des Agent-Loops mit
   eigenem State; die Funktionalität ist im Kern denkbar, aber das konkrete UI-Adapter-
   und Output-Format ist `code_assistant`-spezifisch. Vorschlag: Im Kern nur ein
   abstraktes Trait, alles weitere im Konsumenten.
3. **Statische vs. dynamische Tool-Capabilities.** Statische `&'static [&'static str]`
   sind billig und reichen vermutlich; alternativ ein Bitset / typsichere Capabilities.
4. **Persistenz von `Box<dyn AnyOutput>`.** Heute gelöst über Tool-Name + Registry.
   Beim Crate-Split muss die Persistenz entweder die Konsumenten-Registry kennen
   (übergebbar) oder Tool-Outputs bringen ihre Tags selbst mit.
5. **Streaming-UI-Events vs. Plugin-Events.** Manche heute existierenden UI-Events
   („Worktree Update", „Update Plan") werden vom Agent-Loop ausgelöst. Sie müssen über
   `AgentUiEvent::Custom` reisen. Das vereinheitlicht UI-Strom, kostet aber etwas
   Type-Safety.
6. **Synchrone vs. asynchrone Hooks.** Manche Hooks (z. B. `ToolDispatchPolicy::partition`)
   laufen sehr häufig und sollten synchron bleiben. Andere (`CompactionPolicy::run`)
   müssen async sein. Aktuelle Skizze trifft diese Trennung bereits.
7. **Mehrere Hooks gleichen Typs.** Praktisch nützlich (Composition!). Reihenfolge muss
   deterministisch sein; Vorschlag: `IterationHook`s laufen in Registrierungsreihenfolge,
   `ToolInterceptor`s im First-Match-Wins-Stil.
8. **Naming.** Es gibt heute das Konzept "ToolSyntax" — als externes Crate-Konzept besser
   "InvocationFormat" oder "ToolDialect", weil "Syntax" oft mit Sprach-Syntax verwechselt
   wird.

---

## 8. Was die Refactoring-Investition liefert

- **Wiederverwendbarer Kern:** Andere Anwendungen (z. B. domänenspezifische Assistenten,
  Test-Harnesse, MCP-Wrapper) können den Agent-Loop ohne Fork nutzen.
- **Klarere Verantwortlichkeiten:** Spezialfälle (Plan, Compaction, Sub-Agent, Naming,
  Recovery) leben in eigenen Modulen statt im 2000-Zeiler `runner.rs`.
- **Bessere Testbarkeit:** Jeder Hook ist isoliert testbar; der Kern-Loop hat keine
  Anwendungs-Tools-Tests mehr.
- **Saubere Erweiterung der Tool-Syntaxen:** Drittanbieter können ein Format hinzufügen,
  ohne den Kern zu patchen.
- **Wegfall der globalen Registries:** Mehrere Agenten unterschiedlicher Tool-Sets
  innerhalb eines Prozesses werden möglich (heute teilen alle eine `OnceLock`).
- **Vorbereitung auf SDK-Auslieferung:** Der Kern lässt sich als externes Crate (oder
  als `cargo install agent-core`-Binary für ein „Headless Agent SDK") publishen.

---

## 9. Empfohlene Reihenfolge

1. Phase 1 + 2 (refactoren, ohne Crate-Split): hoher Mehrwert, geringe Risiken, bringt
   die Architektur in eine plug-bare Form.
2. Phase 3 (ToolScope → Capabilities, schema-driven Defaults): mittlerer Aufwand, beendet
   die hartkodierten Tool-Namen-Listen.
3. Phase 4 (Crate-Split): hauptsächlich Verschiebearbeit; danach kann der Kern getrennt
   versioniert werden.
4. Phase 5 (Aufräumen): Singleton-Entfernung, Resource-Files-Move, Sub-Agent-Trennung.

Jede Phase ist intern kompilierbar, testbar und releasebar. Ein Big-Bang-Refactoring
ist nicht nötig.
