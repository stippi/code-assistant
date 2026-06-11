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

> **Wichtig: Text-basierte Tool-Aufrufformate (XML / Caret) gehören NICHT in den Kern.**
> Tools selbst sind heute schon syntax-agnostisch und das soll so bleiben. Was an einem
> bestimmten Format hängt, ist allein die Übersetzung zwischen LLM-Response/Stream und
> abstrakten `ToolRequest`s sowie die Darstellung der Tools im System-Prompt. Der Kern
> definiert dafür ein minimales Trait (siehe §3.7) **und liefert genau eine
> Default-Implementierung mit: natives Tool-Calling über die LLM-API.** „Native" ist
> kein Text-Format, sondern der API-Mechanismus selbst — ein minimaler Konsument muss
> sich daher mit dem Thema Syntax gar nicht befassen (siehe §3.11). Die konkreten
> XML-/Caret-Implementierungen bleiben Bestandteil des `code_assistant`.

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
  **Achtung: ein `complete_task`-Tool existiert nicht** (weder in `tools/impls/` noch in
  `register_default_tools()`). In XML/Caret-Mode lehnt der Parser unbekannte Tools ab,
  der Pfad ist dort unerreichbar. In Native-Mode validiert die Extraktion aber *nicht*
  gegen die Registry (`parse_tool_use_blocks` übernimmt `ToolUse`-Blöcke ungefiltert) —
  der Pfad feuert also, wenn das Modell `complete_task` halluziniert, und beendet den
  Loop dann **stillschweigend mit einem dangling `ToolUse` ohne ToolResult** in der
  History. Vor allem aber ist der Pfad **tragend für die Test-Infrastruktur**:
  `MockLLMProvider::new` (`tests/mocks.rs`) fügt automatisch eine
  `complete_task`-Response als Loop-Terminator ein (siehe §6, Test-Migration). Der Pfad
  kann entfallen, aber erst nachdem die Mocks auf das natürliche Loop-Ende („keine
  Tool-Requests → GetUserInput") umgestellt sind.
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
- **Format-on-save-Pfad** (in der ersten Fassung dieser Analyse übersehen): `execute_tool`
  erkennt, ob ein Tool seinen Input während der Ausführung verändert hat (`input_modified`),
  schickt dann `UpdateToolParameter`-Events und schreibt via
  `update_message_history_with_formatted_tool` / `update_tool_call_in_text_static` den
  Tool-Aufruf **im Message-History-Text um** — mit `get_formatter(tool_syntax)` und den
  Byte-Offsets `ToolRequest::start_offset/end_offset`. Das ist eine harte Dialekt-Kopplung
  mitten im Loop: der Kern braucht dafür eine Dialekt-Fähigkeit „formatiere einen
  ToolRequest zurück in Text" (siehe §3.7). Die Offsets im `ToolRequest` existieren genau
  für diesen Pfad.

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
- `ToolRegistry::register` konsultiert ein **weiteres Singleton**: `ToolsConfig::global()`
  (`tools/core/config.rs`, lädt `tools.json` mit z. B. dem Perplexity-API-Key) für
  `tool.is_available(config)`. Beim Entkoppeln muss die Verfügbarkeits-Konfiguration bei
  der Registrierung injiziert werden.
- Die Registry wird **nicht nur vom Agent-Loop** gelesen: auch die Stream-Prozessoren
  (`ui/streaming/{xml,caret,json}_processor.rs`, jeweils
  `ToolRegistry::global().is_tool_hidden(name, ToolScope::Agent)` — Scope hartcodiert!),
  `tools/core/title.rs` (Title-Templates) und der MCP-Handler (siehe §2.12) greifen auf
  das Singleton zu. „Singleton entfernen" betrifft also auch die UI-Schicht.

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
  (`is_write_tool` ist aktuell `#[allow(dead_code)]`, also ohne Aufrufer.)
- Trait `ToolUseFilter` selbst ist sauber — aber der `SmartToolFilter` wird an den
  Verwendungsstellen **hartcodiert konstruiert** (in `parser_registry.rs` beim Parsen und
  in den XML-/Caret-Stream-Prozessoren), nicht injiziert. Ein Konsument kann den Filter
  heute also nicht austauschen, ohne diese Stellen zu patchen.

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
`crates/code_assistant/src/agent/persistence.rs`; `SessionState` selbst liegt in
`crates/code_assistant/src/session/mod.rs`)

- `SessionState` enthält `message_nodes`, `active_path`, `tool_executions`, `plan`,
  `config: SessionConfig`, `next_request_id`, `model_config: SessionModelConfig`. Die
  Branch-Tree-Struktur ist konzeptuell allgemein, die Felder `plan` und `model_config`
  jedoch nicht.
- `AgentStatePersistence::save_agent_state(state: SessionState)` koppelt also den Trait
  hart an die konkrete Struktur.
- `SerializedToolExecution::deserialize` greift wieder auf `ToolRegistry::global()` zu.

### 2.9 Sub-Agents (`crates/code_assistant/src/agent/sub_agent.rs`)

- `SubAgentRunner` als Trait ist *fast* okay — aber die Signatur
  `run(parent_tool_id, instructions, tool_scope: ToolScope, require_file_references)`
  referenziert die `ToolScope`-Enum. Wenn `ToolScope` durch Capabilities ersetzt wird
  (§3.6), muss diese Signatur mit angepasst werden; in der heutigen Form kann der Trait
  nicht in den Kern wandern.
- Der Default-Runner mischt darüber hinaus sehr viele konkrete Aspekte (Sandbox,
  `DefaultProjectManager`, `SessionConfig`, eigene UI-Adapter,
  `SubAgentToolCall`/`SubAgentOutput`-JSON-Form für Custom-Renderer).
- Der `spawn_agent`-Tool-Output ist eng mit der UI-JSON-Form verknüpft.

### 2.10 Special-Tools mit fester Bedeutung im Loop

Folgende Tool-Namen sind *strings, die im Agent-Loop hartverdrahtet sind*:

| Tool             | Wo verdrahtet                                  | Effekt |
|------------------|-----------------------------------------------|--------|
| `complete_task`  | `manage_tool_execution`                        | bricht Schleife ab — **Tool existiert nicht; nur in Native-Mode erreichbar und von der Test-Infrastruktur als Loop-Terminator genutzt (siehe §2.1)** |
| `name_session`   | `execute_tool` (vor Standard-Pfad)             | setzt `session_name`, kein UI-Update |
| `update_plan`    | nach erfolgreichem `execute_tool`              | speichert Plan-Snapshot in MessageNode |
| `spawn_agent`    | `can_run_in_parallel`, `execute_spawn_agent_parallel` | erlaubt parallele Ausführung & Spezial-UI (nur bei ≥2 read-only-Aufrufen im selben Turn; einzelne laufen sequenziell) |
| `parse_error`    | `agent::types`, `persistence`                  | Pseudo-Tool für Parse-Fehler |

Plus implizit über den `SmartToolFilter`: `read_files`, `list_files`, `list_projects`,
`search_files`, `glob_files`, `web_fetch`, `web_search` (read), `write_file`,
`replace_in_file`, `delete_files` (write), `execute_command` (write).

### 2.11 Resources / Templates

- `resources/compaction_prompt.md` — fester Compaction-Prompt
- `resources/tool_use_intro.md` — Einleitung der System-Prompt-Tool-Beschreibung
- `resources/system_prompts/{default, claude, codex}.md` + `mapping.json` — modell-
  spezifische Basis-Prompts mit `{{syntax}}` und `{{tools}}` Platzhaltern.

### 2.12 MCP-Server als zweiter In-Prozess-Konsument (`crates/code_assistant/src/mcp/handler.rs`)

Der MCP-Handler ist heute schon ein zweiter Konsument der Tool-Infrastruktur — und damit
ein guter Realitätstest für die Extraktion:

- nutzt `ToolRegistry::global()` für `tools/list` und `tools/call`,
- filtert über `ToolScope::McpServer`,
- konstruiert `ToolContext` direkt (mit `plan: None`, `ui: None`, …) und ignoriert
  `input_modified` bewusst.

Jede Änderung an `ToolScope` (→ Capabilities), `ToolContext` (→ Extensions) und der
Registry (→ Instanz) muss den MCP-Handler mit-migrieren. In den Phasen-Plan (§6) ist das
eingearbeitet.

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
├── dialect/
│   ├── mod.rs              (ToolDialect-Trait, StreamProcessor-Trait)
│   └── native.rs           (Default: natives Tool-Calling; heutiger json_processor
│                            als Stream-Prozessor — einzige Implementierung im Kern)
├── persistence.rs          (StatePersistence-Trait, AgentSnapshot)
├── ui.rs                   (AgentUi-Trait, AgentUiEvent — Minimal-Set)
├── permissions.rs          (PermissionMediator-Trait)
└── test_utils/             (feature = "test-utils": ScriptedLLMProvider,
                             RecordingUi, InMemoryPersistence — damit Konsumenten
                             ihre Plugins/Hooks ohne eigene Mock-Schicht testen können;
                             entsteht aus den heutigen `tests/mocks.rs`-Bausteinen)

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
├── tool_dialects/          (NEU: ein Verzeichnis pro Dialekt als „vertikaler Schnitt")
│   ├── mod.rs              (Auswahl-Helfer: ToolSyntax → Box<dyn ToolDialect>;
│   │                        ToolSyntax::Native liefert die Kern-Default-Impl)
│   ├── xml/                (parser.rs, formatter.rs, stream.rs, prompt_docs.rs, tests.rs)
│   └── caret/              (dito — Native lebt als Default im Kern, siehe agent_core)
├── plugins/                (NEU: code-assistant-spezifische Hooks,
│                            Tests jeweils als #[cfg(test)] mod im selben File)
│   ├── plan.rs             (Plan-Tool-Hook)
│   ├── name_session.rs     (Name-Reminder + Spezial-Tool)
│   ├── projects.rs         (File-Trees + AGENTS.md im System-Prompt)
│   ├── compaction.rs       (Threshold + Prompt)
│   ├── prompt_too_long.rs  (Recovery-Strategie)
│   └── sub_agent.rs        (Sub-Agent-Plugin)
├── ui/                     (alle bisherigen UI-Events bleiben hier;
│                            streaming/ ist nach dem Umzug der Prozessoren leer
│                            bis auf generische Anteile wie DisplayFragment)
├── session/                (Branching, SessionInstance, Manager)
└── ...
```

Das genaue Aufteilen kann auch in einer einzigen Crate `agent_core` mit Sub-Modulen
beginnen und später in mehrere Crates gesplittet werden.

**Layout-Prinzipien** (beheben die zwei Haupt-Navigationsprobleme des Ist-Zustands):

1. **Dialekte als vertikale Schnitte statt horizontaler Schichten.** Heute ist ein
   Dialekt über zwei Bäume verschmiert: Parsing/Formatting/Prompt-Doku unter `tools/`
   (`parse.rs`, `formatter.rs`, `parser_registry.rs`, `system_message.rs`) und die
   Stream-Prozessoren unter `ui/streaming/`. Wer „wie funktioniert Caret?" verstehen
   will, muss heute vier Dateien in zwei Verzeichnissen lesen. Im Zielbild liegt alles
   zu einem Dialekt in *einem* Verzeichnis, inklusive seiner Tests.
2. **Tests wohnen beim Code, den sie testen.** Heute sammeln `agent/tests.rs`
   (~2700 Zeilen, enthält auch Parser-Tests) und `tools/tests.rs` (~1300 Zeilen)
   Querschnitts-Bestände; `tests/` enthält daneben Mocks und Integrationstests. Im
   Zielbild: Dialekt-Tests bei den Dialekten, Plugin-Tests bei den Plugins,
   Loop-Tests beim Runner, und `tests/` schrumpft auf echte Integrationstests plus
   die (kleiner werdenden) code-assistant-spezifischen Mocks. Generische Mocks
   (LLM-Provider, UI, Persistence) wandern als `test_utils` in den Kern.

### 3.2 Generischer `Agent`-Kern

Der zentrale Typ wird vom Anwendungs-Zustand entkoppelt:

```rust
// agent_core::agent::runtime
pub struct AgentRuntime<E: AgentExtensions> {
    llm: Box<dyn LLMProvider>,
    tools: Arc<ToolRegistry<E::ToolExt>>,
    dialect: Arc<dyn ToolDialect>,        // ersetzt parser + formatter (siehe §3.7)
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

> **Hinweis:** Das Feld `cancel: &CancellationToken` in der Skizze ist *neue*
> Funktionalität, keine Extraktion. Heute läuft Abbruch über
> `ui.should_streaming_continue()` und (für Sub-Agents) die
> `SubAgentCancellationRegistry`. Für die Extraktion zunächst weglassen und das
> bestehende Verhalten beibehalten; ein Token kann später ergänzt werden.

### 3.5 Hooks / Plugins (das Herzstück)

Die wichtigsten Erweiterungspunkte als kleine Traits, die die Default-Implementation
„passthrough" macht.

> **Typisierungs-Entscheidung:** Die Hook-Traits müssen entweder (a) generisch über
> `E: AgentExtensions` sein (`trait ToolInterceptor<E>`, gespeichert als
> `Box<dyn ToolInterceptor<E>>` in der `HookRegistry<E>`), damit Hooks typsicher auf
> `ctx.extensions: &mut E::State` zugreifen können, oder (b) der `LoopCtx` exponiert den
> App-Zustand nur als `&mut dyn Any`. Die untenstehenden Skizzen lassen den Parameter der
> Lesbarkeit halber weg — Variante (a) ist die Empfehlung; sie infiziert zwar Registry,
> Builder und alle Hook-Definitionen mit dem Generic, bleibt aber objekt-sicher und
> kompiliert ohne Downcasts. (Beispiel §5.3 implementiert bereits gegen
> `LoopCtx<'_, CaExt>`.)

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
    .with_dialect(Box::new(CaretDialect::new()))
    .with_system_prompt(Box::new(CodeAssistantSystemPrompt::new(...)))
    .add_iteration_hook(Box::new(NameSessionReminderHook))
    .add_iteration_hook(Box::new(ProjectInfoHook))
    .add_tool_interceptor(Box::new(NameSessionInterceptor))
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
///
/// Objekt-Sicherheit: Der Trait nimmt bewusst nirgends `&ToolRegistry<...>` entgegen
/// (das wäre eine generische Methode → `Box<dyn ToolDialect>` unmöglich). Stattdessen
/// reicht der Aufrufer vor-gefilterte `ToolSpec`/`ToolDefinition`-Slices durch. Das
/// entkoppelt den Dialekt zugleich vom Registry-Typ.
pub trait ToolDialect: Send + Sync {
    /// Aus einer fertigen LLM-Antwort `ToolRequest`s extrahieren.
    /// `order_offset` zählt bereits extrahierte Tools des Requests weiter (heutige
    /// Parser-Signatur), damit generierte Tool-IDs eindeutig bleiben.
    /// Liefert zusätzlich eine ggf. an der ersten Tool-Stelle abgeschnittene Variante
    /// der Response zurück, damit Folge-Text nach einem Tool-Block nicht ins Transkript
    /// gelangt.
    fn extract_requests(
        &self,
        response: &LLMResponse,
        request_id: u64,
        order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)>;

    /// Einen `ToolRequest` zurück in die Text-Repräsentation dieses Dialekts
    /// formatieren. Der Kern braucht das für den Format-on-save-Pfad (§2.1): wenn ein
    /// Tool seinen Input während der Ausführung ändert, wird der Aufruf im
    /// Message-History-Text (per `start_offset`/`end_offset`) ersetzt.
    fn format_tool_request(&self, request: &ToolRequest) -> Result<String>;

    /// Ob Tool-Ergebnisse als native `ToolResult`-Blöcke zur API reisen (Native)
    /// oder vor dem Request in Text umgewandelt werden müssen (XML/Caret —
    /// heutiges `convert_tool_results_to_text`). Die Umwandlung selbst kann der Kern
    /// übernehmen, da er die gerenderten Tool-Outputs kennt; der Dialekt liefert nur
    /// die Entscheidung.
    fn uses_native_tool_results(&self) -> bool;

    /// Ein Stream-Prozessor, der `StreamingChunk`s in `DisplayFragment`s übersetzt.
    /// `hidden_tools` ersetzt den heutigen Singleton-Zugriff der Prozessoren
    /// (`ToolRegistry::global().is_tool_hidden(name, ToolScope::Agent)` — Scope dort
    /// heute sogar hartcodiert): der Aufrufer reicht ein Prädikat oder eine
    /// Namens-Menge der versteckten Tools herein.
    fn stream_processor(
        &self,
        ui: Arc<dyn AgentUi>,
        request_id: u64,
        hidden_tools: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Box<dyn StreamProcessor>;

    /// Wie der Dialekt die LLM-Tool-Liste in den `LLMRequest` einspeist:
    /// - Native: `Some(tool_definitions)` — die LLM-API kennt die Tools nativ.
    /// - XML / Caret: `None` — die Tools werden im System-Prompt beschrieben.
    fn populate_request_tools(&self, tools: &[ToolDefinition]) -> Option<Vec<ToolDefinition>>;

    /// Optional: Block für die Tool-Doku im System-Prompt. `None` für Native.
    fn render_tool_section_for_prompt(&self, tools: &[ToolDefinition]) -> Option<String>;

    /// Optional: Format-Beschreibung („So rufst du Tools auf …"). `None` für Native.
    fn render_format_section_for_prompt(&self) -> Option<String>;

    /// Erkennt, ob eine bereits gespeicherte Nachricht einen Tool-Aufruf in *diesem*
    /// Dialekt enthält (für Normalisierung beim Laden des Verlaufs).
    fn message_contains_invocation(&self, message: &Message) -> bool;
}
```

Damit liegt die heutige `parser_registry` / `formatter` / `system_message`-Maschinerie
für die Text-Formate im Konsumenten. Der `code_assistant` liefert `XmlDialect` und
`CaretDialect` als interne Implementierungen und wählt zur Laufzeit anhand der
Session-Konfiguration (`ToolSyntax`) eine aus — `ToolSyntax::Native` mappt auf die
Default-Implementierung des Kerns.

Was der Kern liefert:

- Trait `ToolDialect` und Trait `StreamProcessor` (klein, syntaxneutral).
- **Genau eine Default-Implementierung: natives Tool-Calling** (`dialect/native.rs`).
  Sie ist trivial — `ToolUse`-Blöcke durchreichen, `populate_request_tools` liefert die
  Tool-Definitionen, keine Prompt-Doku, Stream-Prozessor ist der heutige
  `json_processor` — und sie ist das, was praktisch jeder Drittkonsument will.
- Der `AgentRuntimeBuilder` akzeptiert optional einen `Box<dyn ToolDialect>`;
  ohne Angabe gilt der Native-Default.
- Der `SystemPromptProvider` (siehe §3.5) bekommt den Dialekt durchgereicht und kann
  ihn nach `render_format_section_for_prompt` und `render_tool_section_for_prompt`
  fragen, wenn er den System-Prompt aufbaut.

Damit gilt:

- **Keine `ToolSyntax`-Enum im Kern.** Sie bleibt als Auswahl-Helfer (CLI-Argument,
  Session-Konfiguration, Persistenz) im `code_assistant` — dort ist der Name etabliert
  und serialisiert, er wird **nicht umbenannt**.
- **Keine globale `ParserRegistry`** mehr. Es gibt einfach immer genau einen Dialekt,
  der pro Agent-Instanz gesetzt wird.
- **Kein `agent_tools_syntax`-Crate.** Die heutigen XML-/Caret-Implementierungen
  wandern als interne Module in den `code_assistant` (unter `tool_dialects/`).
  Wenn jemand sie wirklich teilen will, kann das später ein optionales Helfer-Crate
  werden — aber das ist ausdrücklich kein Pflichtteil.
- **Tools selbst bleiben dialektfrei.** Sie kennen weiterhin nur ihr JSON-Schema und
  ihren `Render`-Output. `multiline_params`, Schema-`examples` etc. sind reine
  Zusatz-Metadaten für Text-Dialekte — ein Konsument, der beim Native-Default bleibt,
  kann sie vollständig ignorieren.

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

### 3.11 Konsumenten-Sicht: Was ein minimaler Einbau braucht

Der Lackmustest für die Architektur: Wie fühlt sich der Kern in einem *fremden* Projekt
an, das nur eigene Tools einhängen will? Zielbild — drei Dinge sind Pflicht, alles
andere hat Defaults:

```rust
// 1. Tool implementieren — kein Syntax-/Dialekt-Wissen nötig.
//    Pflicht: Input (Deserialize), Output (Serialize + Render + ToolResult), spec(), execute().
struct QueryDatabaseTool;

#[async_trait]
impl Tool for QueryDatabaseTool {
    type Input = QueryInput;          // serde-Struct, beschreibt das JSON-Schema
    type Output = QueryOutput;        // Render::render() = was das LLM als Ergebnis sieht
    fn spec(&self) -> ToolSpec { /* name, description, parameters_schema, capabilities */ }
    async fn execute(&self, ctx: &mut ToolContext<'_, MyExt>, input: &mut QueryInput)
        -> Result<QueryOutput> { ... }
}

// 2. Registry-Instanz befüllen (kein Singleton, keine Default-Tools).
let mut tools = ToolRegistry::new();
tools.register(Box::new(QueryDatabaseTool));

// 3. Runtime bauen — kein Dialekt, keine Hooks, keine Interceptors nötig.
let runtime = AgentRuntimeBuilder::new(llm_provider)
    .with_tools(tools)
    .with_system_prompt_text("You are a database assistant ...")
    .with_ui(my_ui)                   // oder Default: No-op-UI für Headless-Betrieb
    .build();
```

Dabei gilt:

- **Syntax ist komplett ignorierbar.** Ohne `with_dialect(...)` läuft natives
  Tool-Calling über die LLM-API (§3.7). Tool-Implementierungen berühren das Thema nie —
  Dialekte konsumieren ausschließlich die `ToolSpec`-Daten. Dieselbe Registry läuft
  unverändert unter XML/Caret, wenn der Konsument später doch einen Text-Dialekt setzt.
  Syntax und Tools sind also auch in der Konfiguration vollständig orthogonal
  (getrennte Builder-Aufrufe).
- **`ToolContext`-Extension nur bei Bedarf.** Tools, die keine App-Dienste brauchen,
  verwenden `Ext = ()`. Wer eigene Dienste (DB-Pool, eigene Config) braucht, definiert
  einen Ext-Typ — das ist die einzige Stelle, an der der Konsument mit den Generics
  des Kerns in Kontakt kommt, solange er keine Hooks schreibt.
- **Alle übrigen Bausteine haben neutrale Defaults:** Hooks = passthrough,
  Persistence = In-Memory, Compaction = aus, Recovery = generische
  Streaming-Retry-Heuristik, `PermissionMediator` = None.
- **Konsequenz für den Kern-`ToolContext`:** `command_executor` sollte dort `Option`
  sein (oder in die Extension wandern) — ein Konsument ohne Shell-Tools soll keinen
  `CommandExecutor` stellen und die Crate-Abhängigkeit nicht ziehen müssen. Die
  Skizze in §3.4 ist entsprechend zu lesen.

Was der Konsument bewusst *nicht* sieht: `ToolSyntax`, Parser, Formatter,
Stream-Prozessoren für Text-Formate, Multiline-/Beispiel-Metadaten, Capability-Scoping
(solange er alle Tools immer anbietet) und sämtliche code-assistant-Plugins.

---

## 4. Zuordnung der heutigen Spezial-Pfade auf Hooks

| Heute (im `Agent`)                                   | Ziel                                       |
|------------------------------------------------------|---------------------------------------------|
| `complete_task`-Spezial im `manage_tool_execution`   | **löschen, nachdem die Test-Mocks umgestellt sind** (siehe §2.1 und §6 Phase 1); `InterceptOutcome::BreakLoop` bleibt als Hook-Möglichkeit erhalten |
| `name_session`-Spezial in `execute_tool`             | `ToolInterceptor::Handled { hidden_in_ui: true }` + `IterationHook::shape_request` für Reminder |
| `update_plan` → Plan-Snapshot                        | `ToolInterceptor::after_success` Variante / `IterationHook::after_iteration` |
| `spawn_agent` parallel mit `mode=read_only`          | `ToolDispatchPolicy::partition` im Plugin   |
| `inject_naming_reminder_if_needed`                   | `IterationHook::shape_request`              |
| `convert_tool_results_to_text` (XML/Caret)           | Kern-Funktion, gesteuert über `ToolDialect::uses_native_tool_results()` (§3.7) |
| Format-on-save (`update_message_history_with_formatted_tool`, `notify_tool_parameter_updates`) | Kern-Funktion; nutzt `ToolDialect::format_tool_request` (§3.7) |
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
    pub dialect: &'a dyn ToolDialect,
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

0. `complete_task`-Pfad entfernen (siehe §2.1) — als Vorab-Commit. Eine Analyse von
   `agent/tests.rs` zeigt, dass die Tests bereits in zwei Gruppen zerfallen:
   - **Tests, die schon heute über eine Text-Response enden** (Compaction-Tests,
     Prompt-too-long-Test, `test_write_file_outside_root_…` mit expliziter
     `completion_response`): bei ihnen wird die auto-eingefügte `complete_task`-Response
     **nie konsumiert** — sie liegt als totes Gewicht unten im Response-Stack. Diese
     Tests beweisen, dass das natürliche Loop-Ende („Antwort ohne Tools →
     GetUserInput") als Test-Terminator funktioniert. Keine Änderung nötig.
   - **Tests, die nach einer Tool-Ausführung enden** (`test_unknown_tool_…`,
     `test_invalid_xml_…`, `test_parse_error_…`, …): hier dient die auto-eingefügte
     Response tatsächlich als Loop-Terminator. Diese Tests bekommen eine explizite
     finale Text-Response (`create_test_response_text("…")`) in ihre Response-Liste.

   Empfehlung daher: das **Auto-Insert in `MockLLMProvider::new` ersatzlos streichen**
   statt es auf eine Text-Response umzubauen — jeder Test deklariert seine vollständige
   Response-Sequenz selbst. Das Magic-Insert kostet heute Verständnis (Kommentare wie
   „popped last before complete_task" in den Tests belegen das), und der Mock schlägt
   bei erschöpften Responses ohnehin laut fehl („No more mock responses"), so dass ein
   vergessener Terminator sofort auffällt. Assertions auf `requests.len()` bleiben
   unverändert gültig (gleiche Anzahl LLM-Aufrufe); nur die letzte Assistant-Message
   in der History ist dann Text statt eines dangling `ToolUse`.

   Danach den Pfad in `manage_tool_execution` löschen (plus den
   `complete_task`-Eintrag in `ui/gpui/shared/file_icons.rs`).
1. Spezial-Pfade aus `Agent::run_single_iteration` in private Methoden extrahieren:
   `intercept_special_tool`, `apply_naming_reminder`, `partition_parallel_tools`,
   `compaction_policy`, `recovery_policy` — und den Format-on-save-Pfad
   (`update_message_history_with_formatted_tool` + `notify_tool_parameter_updates`)
   als eigene Einheit isolieren. Verhalten bleibt gleich.
2. Globale `ToolRegistry::global()`-Aufrufe in Parser/Persistence durch ein injizierbares
   Argument ersetzen (Compatibility-Wrapper, der weiter `global()` nutzt, bleibt vorerst).
   Gleiches gilt für die Aufrufe in den Stream-Prozessoren und `title.rs` (§2.3) — dort
   genügt zunächst ein injiziertes „is hidden"-Prädikat statt der ganzen Registry.
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
2. `SmartToolFilter` und Sub-Agent-Logik auf Capabilities umstellen. Dabei auch die
   `SubAgentRunner::run`-Signatur anpassen (sie nimmt heute `tool_scope: ToolScope`,
   siehe §2.9) sowie die hartcodierte `ToolScope::Agent`-Annahme in den
   Stream-Prozessoren auflösen.
3. Multiline-Parameter und Doku-Beispiele aus dem JSON-Schema ableiten; `is_multiline_param`
   und „magic placeholder names" entfernen.
4. `ToolScope`-Enum entfernen oder zu `enum ToolScope(&'static str)` (just-a-tag) reduzieren.
   Das schließt den MCP-Handler ein (`ToolScope::McpServer` → Capability-Filter, §2.12).

### Phase 4 — Crate-Split

1. Neues Crate `agent_tools_core` anlegen, dorthin verschieben:
   `tools/core/{tool, dyn_tool, registry, render, result, spec, title}.rs`.
2. Neues Crate `agent_core`: `agent/runner.rs`, neue Hook-Module, `MessageTree`,
   `AgentUiEvent`-Minimum, `StatePersistence`-Trait, abstraktes `ToolDialect`-Trait
   und `StreamProcessor`-Trait, dazu die **Native-Default-Implementierung** (inkl.
   heutigem `json_processor` als Stream-Prozessor) — *aber keine XML-/Caret-
   Implementierungen*.
3. `code_assistant`:
   - Verschiebt `tools/{parse, formatter, parser_registry, system_message}.rs` und
     `ui/streaming/{xml,caret}_processor.rs` in ein internes Modul
     `tool_dialects/` — organisiert **pro Dialekt** (`xml/`, `caret/`, siehe
     Layout-Prinzipien in §3.1) — und implementiert dort `ToolDialect` +
     `StreamProcessor` für XML und Caret. (`json_processor.rs` wandert als Teil der
     Native-Default-Implementierung in den Kern, siehe Schritt 2.) Die zugehörigen
     Tests (`*_processor_tests.rs`, `tools/tests.rs`-Anteile, Parser-Tests aus
     `agent/tests.rs`) ziehen mit um. Diese Implementierungen bleiben Anwendungs-intern.
   - `tool_use_filter.rs` (`SmartToolFilter`) bleibt im `code_assistant` — wertet
     nach dem Refactoring Capability-Tags statt Tool-Namen aus (vgl. Phase 3).
   - Implementiert `AgentExtensions` (`CaExt`, Snapshot, ToolExt).
   - Stellt den MCP-Handler auf die Registry-Instanz und den neuen `ToolContext`
     (mit `CaExt`) um — er ist der zweite In-Prozess-Konsument (§2.12).
   - Verschiebt `UiEvent` in eine `code_assistant_ui`-Lib oder belässt sie und packt sie
     als `AgentUiEvent::Custom`.
   - Behält Branching, Sub-Agents, Sessions, Persistence-Files.
4. Optional: `agent_persistence` mit JSON-File-Adapter herausziehen.

### Phase 5 — Aufräumen

1. `ToolRegistry::global()` entfernen; alle Aufrufer bekommen die Registry per Argument
   oder über den `ToolContext`/`LoopCtx`. Aufruferliste umfasst neben Agent-Loop und
   Parser auch: Stream-Prozessoren, `title.rs`, `formatter.rs`, MCP-Handler und
   `SerializedToolExecution::deserialize`. Ebenso `ToolsConfig::global()` auflösen —
   die Verfügbarkeits-Konfiguration wird beim Befüllen der Registry-Instanz übergeben.
2. `ParserRegistry`-Singleton ersatzlos streichen — pro Agent existiert genau ein
   `Box<dyn ToolDialect>`, das beim Bauen gesetzt wird. Die `ToolSyntax`-Enum entfällt
   im Kern; im `code_assistant` darf sie als interner Auswahl-Helfer für die mitgelieferten
   Dialekte bestehen bleiben.
3. Sub-Agent-spezifische UI-Adapter ins `code_assistant` verschieben; im Kern bleibt
   nur `SubAgentRunner`-Trait — und auch der ist optional.
4. Resources (`compaction_prompt.md`, `tool_use_intro.md`, `system_prompts/*.md`) in
   den `code_assistant` verschieben (keine Default-Prompts im Kern).

### Test-Migration (Querschnittsthema über alle Phasen)

Die bestehenden Tests sind das wichtigste Sicherheitsnetz des Refactorings — sie müssen
pro Phase mitgedacht werden, nicht am Ende:

- **`agent/tests.rs` (~2700 Zeilen) ist ein Mischbestand** und sollte beim Refactoring
  entflochten werden:
  - Die ersten ~365 Zeilen (`test_flexible_xml_parsing`, `test_replacement_xml_parsing`,
    `test_mixed_tool_start_end`, `test_ignore_non_tool_tags`, …) sind **reine
    XML-Parser-Tests**, die gar keinen `Agent` konstruieren — sie gehören zu den
    Dialekt-Tests und ziehen mit nach `tool_dialects/` um (Phase 4, oder schon früher
    als kostenloser Aufräum-Commit).
  - Der Rest treibt den Loop über die öffentliche `Agent`-API mit `MockLLMProvider`,
    `MockStatePersistence` und Mock-UI (überwiegend `ToolSyntax::Native`, einzelne
    XML-/Caret-Fälle). Phase 1+2 lassen diese API unverändert — diese Tests bleiben als
    Regressionsnetz bestehen. Einzige Vorab-Änderung: der `complete_task`-Terminator
    bzw. das Auto-Insert in `MockLLMProvider::new` (siehe Phase 1, Schritt 0).
- **`ui/streaming/{xml,caret,json}_processor_tests.rs` + `test_utils.rs`** sind bereits
  sauber pro Prozessor getrennt — sie wandern in Phase 4 unverändert mit den Prozessoren
  nach `tool_dialects/`.
- **Direkte `ToolContext`-Konstruktion** gibt es an genau zwei Test-Stellen: dem
  `#[cfg(test)]`-Konstruktor `ToolContext::new` und `tests/format_on_save_tests.rs`.
  Jede `ToolContext`-Änderung (Phase 1.3 `dyn Any`-Backdoor, Phase 4 generisches `Ext`)
  betrifft genau diese beiden Stellen; den Test-Konstruktor als einzigen Einstiegspunkt
  beibehalten und mitziehen.
- **Unit-Tests in den Tool-Impls** (`read_files.rs`, `view_images.rs`,
  `view_documents.rs`, …) holen sich `ToolRegistry::global()`. Beim Umstieg auf
  Registry-Instanzen (Phase 4/5) bauen sie sich stattdessen eine lokale Registry mit nur
  den benötigten Tools — mechanische Änderung und zugleich ein Gewinn an Test-Isolation
  (heute teilen alle Tests den `OnceLock`-Zustand inkl. `ToolsConfig`).
- **`tools/tests.rs` (Parser-/Formatter-Tests, ~1300 Zeilen)** sind faktisch
  Dialekt-Tests: sie wandern in Phase 4 zusammen mit dem Code nach
  `code_assistant::tool_dialects/` (kein Umschreiben, nur Verschieben + Pfade).
- **`system_message.rs`-Tests** referenzieren `ToolScope::Agent` und die
  `ParserRegistry` — sie werden in Phase 3 (Capabilities) bzw. Phase 4 (Dialekt statt
  Registry) angepasst.
- **`MockStatePersistence`** implementiert `AgentStatePersistence` gegen das konkrete
  `SessionState`; beim Umstieg auf den snapshot-generischen `StatePersistence`-Trait
  (Phase 4) wird er zum generischen In-Memory-Adapter — gehört dann sinnvollerweise als
  Test-Helfer in den Kern (`agent_core`), damit Drittkonsumenten ihn mitnutzen können.
- **Neu hinzukommend:** isolierte Unit-Tests pro Plugin/Hook (Phase 2) — das ist ein
  Teil des in §8 versprochenen Testbarkeits-Gewinns und sollte direkt beim Umzug der
  Spezial-Pfade entstehen, solange das alte Verhalten als Referenz danebenliegt.

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
8. **Naming — entschieden.** Die `ToolSyntax`-Enum behält ihren Namen und bleibt im
   `code_assistant` (CLI-Argument, Session-Konfiguration, serialisierte Felder — ein
   Rename hätte nur Migrations-Kosten). Das neue Kern-Trait heißt `ToolDialect`, weil
   es mehr als Syntax bündelt (Parsing, Rück-Formatierung, Streaming, Prompt-Doku,
   Request-Bestückung) und „Native" gar keine Text-Syntax hat. Die beiden Namen
   koexistieren an der Grenze: `ToolSyntax` ist Konfigurations-Vokabular des
   Konsumenten, `ToolDialect` die Kern-Abstraktion.
9. **Generics-Budget.** `AgentExtensions` mit drei Associated Types macht
   `AgentRuntime<E>`, `HookRegistry<E>`, `ToolRegistry<E::ToolExt>` und alle Hook-Traits
   generisch (vgl. Hinweis in §3.5). Das ist typsicher, aber der teuerste Teil des
   Designs. Vor Phase 4 bewusst entscheiden, ob alle drei Typ-Parameter nötig sind —
   z. B. könnte der Kern immer einen festen `CoreSnapshot` persistieren (§3.9) und
   App-Felder dem Persistence-Adapter des Konsumenten überlassen; dann entfällt
   `E::Snapshot`. Phase 1–3 erzwingen noch keine dieser Entscheidungen.

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
