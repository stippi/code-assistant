
# Making Tool Blocks Pluggable – Towards a “Triple-Caret” Syntax

> **Status** Draft • 2024-06-XX
> **Authors** `assistant` ↔ `architects@code-assistant`

---

## 1 Motivation

The agent currently understands **two hard-wired invocation syntaxes**

| Syntax | Entry-point code |
|--------|-----------------|
| _XML-ish tags_ (`&lt;tool:…&gt;`) | `ui/streaming/xml_processor.rs` + state-machine in `tools::parse_xml_tool_invocations` |
| _Native JSON function calls_ | `ui/streaming/json_processor.rs` + direct `ToolUse` blocks via provider API |

Because these paths are duplicated, adding a **third style** requires churning multiple modules.
We want to:

1. Add a **“triple-caret fenced block”** syntax that is cheaper in tokens and easier to type.
2. Make the parsing/streaming layer **pluggable**, so future syntaxes (YAML, TOML, etc.) drop in with minimal risk to existing code.

---

## 2 Existing Architecture (quick recap)

```
agent/runner.rs
  ├─ parse_llm_response()  – extracts ToolRequests from assistant output
  │     ↳ tools::parse_xml_tool_invocations()   (XML mode)
  └─ create_stream_processor()  – maps ToolMode ➜ {Xml,Json}StreamProcessor
         ↳ ui/streaming/{xml,json}_processor.rs
```

Coupling points:

* `ToolMode` enum enumerates only *Native* and *Xml*.
* `create_stream_processor` and `parse_llm_response` branch on that enum.
* Each processor is a *bespoke* state machine.

---

## 3 Triple-Caret Block — Specification (v1)

````text
^^^write_file
project: code-assistant
path: src/lib.rs
content ---
//! hello
fn main() {}
--- content
^^^
````

* Leading & trailing fence: `^^^` at column 0.
* Opening fence MUST include the *tool name* after the fence.
* Header contains single-line parameters as `key: value` pairs.
* Multi-line parameters use `paramname ---` to start and `--- paramname` to end the block.
* Multiple multi-line parameters are supported in the same tool invocation.
* Exactly one tool block per assistant message (maintains current safety property).

This format:

* costs ~8 tokens for the wrapper vs ~20 with XML,
* eliminates indentation errors (no YAML-style indentation required),
* supports multiple multi-line parameters cleanly,
* can be parsed with straightforward regex patterns,
* is human-friendly (copy/paste into editors without formatting concerns).

---

## 4 Extensibility Design

### 4.1 Introduce a Parser Trait

```rust
pub trait ToolInvocationParser: Send + Sync {
    /// Extract `ToolRequest`s from a complete LLM response.
    /// Implementations may inspect either the raw text blocks, the `ToolUse`
    /// blocks, or both.
    fn extract_requests(
        &mut self,
        response: &llm::LLMResponse,
        req_id: u64,
        order_offset: usize,
    ) -> anyhow::Result<Vec<ToolRequest>>;

    /// A stream-processor that renders *this syntax* for the UI.
    fn stream_processor(
        &self,
        ui: Arc<Box<dyn UserInterface>>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait>;
}
```

### 4.2 Registry

#### Handling array-valued parameters

Array parameters defined in the canonical JSON spec are expressed using
explicit bracket notation:

```text
^^^read_files
project: my_proj
paths: [
src/main.rs
Cargo.toml
docs/README.md
]
^^^
```

**Array Syntax Rules:**
- Arrays are enclosed in `[` and `]`
- One element per line
- No indentation required (consistent with multi-line parameter design)
- Empty lines within arrays are ignored
- No comma-separated single-line arrays

The caret parser converts bracket arrays into `serde_json::Value::Array` so the
produced `ToolRequest::input` matches the existing schema. Supplying a single
scalar value is still supported for back-compatibility.

```rust
enum ToolSyntax { Xml, Json, Caret }

struct ParserRegistry { … }

impl ParserRegistry {
    fn global() -> &'static Self { … }
    fn get(&self, mode: ToolSyntax) -> &'static dyn ToolInvocationParser;
}
```

### 4.3 Refactor call-sites

* `agent::ToolMode` → rename to `ToolSyntax` and add `Caret`.
* `create_stream_processor()` becomes `ParserRegistry::get(mode).stream_processor(…)`.
* `parse_llm_response()` delegates to `ParserRegistry::get(mode).parse(…)`.

### 4.4 Implement `CaretParser`

A thin implementation that:

1. Buffers text until it sees `^^^`.
2. Splits header / body.
3. Emits `ToolRequest { id, name, input }` with the same `id` generator as XML path.

Streaming processor: trivial—no incremental param display required; whole block arrives in one shot → emit fragments `ToolName / ToolParameter* / ToolEnd`.

---

## 5 Migration & Compatibility

* Default remains **XML** to avoid breaking any existing prompt templates.
* Native JSON path unchanged.
* Caret mode is **opt-in** via CLI flag or environment variable:
  `--tool-syntax caret` or in config file.

---

## 6 Implementation Steps ✅ COMPLETED

1. [x] **Extract** common behaviours from `xml_processor.rs` / `json_processor.rs` into the trait. ✅
2. [x] **Move** XML & JSON processors behind `XmlParser` / `JsonParser` that implement the trait. ✅
3. [ ] **Add** `CaretParser`. ⏳ **NEXT SESSION**
4. [x] **Update** `ToolMode` → `ToolSyntax`; adjust CLI flag parsing. ✅
5. [x] **Wire** `agent::runner` to registry. ✅
6. [ ] Write **unit tests** for `CaretParser`. ⏳ **NEXT SESSION**
7. [ ] Document in `README.md`. ⏳ **NEXT SESSION**

### 🎉 **Status: Foundation Complete!**

**Implemented in Current Session:**
- ✅ **Parser Registry** with `ToolInvocationParser` trait
- ✅ **XmlParser** and **JsonParser** implementations
- ✅ **Agent Runner** refactored to use registry
- ✅ **ToolSyntax** rename completed (was `ToolMode`)
- ✅ **CLI harmonized** to `--tool-syntax`
- ✅ **Message conversion architecture** cleaned up
- ✅ **All tests passing** (10/10 agent tests, 43/43 streaming tests)

**Architecture Benefits Achieved:**
- 🔧 **Pluggable Parsers** - New syntaxes can be added without touching core logic
- 🧪 **Isolated Testing** - Each parser can be tested independently
- 📈 **Consistent Interface** - All parsers implement same trait
- 🔄 **Backward Compatible** - Existing XML/JSON functionality unchanged

**Ready for Next Session:**
- 🚀 Add `ToolSyntax::Caret` enum variant
- 🚀 Implement `CaretParser` with regex-based parsing
- 🚀 Create `CaretStreamProcessor` for UI rendering
- 🚀 Write comprehensive tests for caret syntax

---

## 7 Appendix — Quick Examples

### read_files (single parameter)

````text
^^^read_files
project: my_proj
path: src/main.rs
^^^
````

### read_files (array parameter)

````text
^^^read_files
project: my_proj
paths: [
src/main.rs
Cargo.toml
docs/README.md
]
^^^
````

### replace_in_file (multi-line diff parameter)

````text
^^^replace_in_file
project: cool_proj
path: src/lib.rs
diff ---
[SEARCH/REPLACE block content here]
--- diff
^^^
````

### write_file (multi-line content parameter)

````text
^^^write_file
project: notes
path: design.md
content ---
# Title
Multiline
body.
--- content
^^^
````

### Multiple multi-line parameters example

````text
^^^replace_in_file
project: my_proj
path: src/main.rs
diff ---
[SEARCH/REPLACE block for code changes]
--- diff
comment ---
This change updates the function name
to better reflect its purpose.
--- comment
^^^
````

---

**Result** – we keep the single-tool-per-turn safety contract while allowing any number of syntaxes to coexist, decoupled from the core agent loop.

---

## 8 Konkrete Änderungsstellen im Code

Nach Analyse des bestehenden Codes sind folgende spezifische Änderungen erforderlich:

### 8.1 Core-Typen erweitern

**Datei:** `crates/code_assistant/src/types.rs:230-235`
- `ToolMode` enum um `Caret` Variante erweitern
- Eventuell umbenennen zu `ToolSyntax` für bessere Klarheit
- `ValueEnum` Implementation entsprechend anpassen

### 8.2 Agent Runner anpassen

**Datei:** `crates/code_assistant/src/agent/runner.rs`

**Funktionen die geändert werden müssen:**
- `parse_and_truncate_llm_response()` (Zeile ~680): Aktuell fest verdrahtet mit `parse_xml_tool_invocations_with_truncation()`
- `create_stream_processor()` call (Zeile ~420): Verwendet `self.tool_mode` direkt
- `get_next_assistant_message()` (Zeile ~420ff): Branching auf `self.tool_mode` für Tool-Definition

**Konkrete Änderungen:**
```rust
// Statt direktem branching auf ToolMode:
match self.tool_mode {
    ToolMode::Native => messages,
    ToolMode::Xml => self.convert_tool_results_to_text(messages),
}

// Registry-basierter Ansatz:
let parser = ParserRegistry::get(self.tool_mode);
let converted_messages = parser.convert_messages_for_llm(messages);
```

### 8.3 Tool Parsing erweitern

**Datei:** `crates/code_assistant/src/tools/parse.rs`

**Neue Funktionen hinzufügen:**
- `parse_caret_tool_invocations()` - analog zu `parse_xml_tool_invocations()`
- `parse_caret_tool_invocations_with_truncation()` - analog zu bestehender XML-Funktion
- `find_first_caret_tool_end_and_truncate()` - analog zu `find_first_tool_end_and_truncate()`

**Bestehende Funktionen anpassen:**
- `parse_and_truncate_llm_response()` in `runner.rs` muss auf Parser-Registry umgestellt werden

### 8.4 Streaming Processors erweitern

**Datei:** `crates/code_assistant/src/ui/streaming/mod.rs`

**Neue Trait einführen:**
```rust
pub trait ToolInvocationParser: Send + Sync {
    fn extract_requests(
        &mut self,
        response: &llm::LLMResponse,
        req_id: u64,
        order_offset: usize,
    ) -> anyhow::Result<Vec<ToolRequest>>;

    fn stream_processor(
        &self,
        ui: Arc<Box<dyn UserInterface>>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait>;
}
```

**Factory Funktion erweitern:**
```rust
// Statt:
pub fn create_stream_processor(
    tool_mode: ToolMode,
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,
) -> Box<dyn StreamProcessorTrait>

// Registry-basiert:
pub fn create_stream_processor(
    tool_syntax: ToolSyntax,
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,
) -> Box<dyn StreamProcessorTrait> {
    ParserRegistry::get(tool_syntax).stream_processor(ui, request_id)
}
```

### 8.5 Neue Dateien erstellen

**Neue Dateien:**
- `crates/code_assistant/src/ui/streaming/caret_processor.rs`
- `crates/code_assistant/src/ui/streaming/caret_processor_tests.rs`
- `crates/code_assistant/src/tools/parser_registry.rs`

### 8.6 CLI Integration

**Datei:** `crates/code_assistant/src/main.rs` (vermutlich)
- CLI-Argument für `--tool-syntax` erweitern um `caret` Option
- Config-Parsing entsprechend anpassen

### 8.7 Systemprompte anpassen

**Datei:** `crates/code_assistant/resources/system_message_tools.md`
- Dokumentation für Caret-Syntax hinzufügen
- Beispiele für beide Syntaxformen bereitstellen

### 8.8 Konkrete Parser-Implementierung

**Caret Tool Parsing Logik:**
```rust
// Regex für Caret-Block-Erkennung:
static CARET_TOOL_REGEX: &str = r"(?m)^\^\^\^([a-zA-Z0-9_]+)$";
static MULTILINE_START_REGEX: &str = r"(?m)^([a-zA-Z0-9_]+)\s+---\s*$";
static MULTILINE_END_REGEX: &str = r"(?m)^---\s+([a-zA-Z0-9_]+)\s*$";

// Parsing-Algorithmus:
1. Regex-Match für `^^^tool_name`
2. Header-Bereich: `key: value` Zeilen als einzeilige Parameter parsen
3. Multi-line-Bereiche: `paramname ---` bis `--- paramname` als mehrzeilige Parameter
4. Array-Parameter: sowohl YAML-Listen als auch wiederholte Keys/Blöcke unterstützen
5. Keine implizite `content`-Behandlung - alle Parameter explizit definiert
```

### 8.9 Testing-Infrastruktur

**Bestehende Test-Patterns erweitern:**
- `crates/code_assistant/src/ui/streaming/xml_processor_tests.rs` als Template
- `crates/code_assistant/src/ui/streaming/json_processor_tests.rs` als Template
- `crates/code_assistant/src/tools/tests.rs` erweitern

### 8.10 Backward Compatibility

**Wichtige Kompatibilitäts-Überlegungen:**
- Bestehende Prompts und Konfigurationen müssen weiterhin funktionieren
- Default bleibt XML-Syntax
- Migrations-Pfad für bestehende Sessions berücksichtigen
- Feature-Flag oder graduelle Einführung erwägen

### 8.11 Zusätzliche Fundstellen

**Import-Statements überprüfen:**
- Alle Module die `ToolMode` importieren müssen entsprechend angepasst werden
- Besonders `use super::ToolMode;` Statements in verschiedenen Dateien

**System Message Templating:**
- `crates/code_assistant/src/agent/runner.rs:get_system_prompt()` - Auswahl des korrekten System-Prompts basierend auf Tool-Syntax
- Template-Ersetzung `{{tools}}` muss für alle Syntax-Modi funktionieren

**Error Handling:**
- `crates/code_assistant/src/agent/runner.rs:format_error_for_user()` - Error-Messages müssen syntax-spezifisch sein
- Tool-Result-Konvertierung in verschiedenen Modi
