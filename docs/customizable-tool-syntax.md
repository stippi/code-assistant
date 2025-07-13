
# Making Tool Blocks Pluggable – Towards a “Triple-Caret” Syntax

> **Status** Draft • 2024-06-XX
> **Authors** `assistant` ↔ `architects@code-assistant`

---

## Motivation

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

## Existing Architecture (quick recap)

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

## Triple-Caret Block — Specification (v1)

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

## Extensibility Design

### Introduce a Parser Trait

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

### Registry

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

### Refactor call-sites

* `agent::ToolMode` → rename to `ToolSyntax` and add `Caret`.
* `create_stream_processor()` becomes `ParserRegistry::get(mode).stream_processor(…)`.
* `parse_llm_response()` delegates to `ParserRegistry::get(mode).parse(…)`.

### Implement `CaretParser`

A thin implementation that:

1. Buffers text until it sees `^^^`.
2. Splits header / body.
3. Emits `ToolRequest { id, name, input }` with the same `id` generator as XML path.

Streaming processor: trivial—no incremental param display required; whole block arrives in one shot → emit fragments `ToolName / ToolParameter* / ToolEnd`.

---

## Migration & Compatibility

* Default remains **XML** to avoid breaking any existing prompt templates.
* Native JSON path unchanged.
* Caret mode is **opt-in** via CLI flag or environment variable:
  `--tool-syntax caret` or in config file.

---

## Implementation Steps ✅ COMPLETED

1. [x] **Extract** common behaviours from `xml_processor.rs` / `json_processor.rs` into the trait. ✅
2. [x] **Move** XML & JSON processors behind `XmlParser` / `JsonParser` that implement the trait. ✅
3. [x] **Add** `CaretParser`. ✅ **COMPLETED**
4. [x] **Update** `ToolMode` → `ToolSyntax`; adjust CLI flag parsing. ✅
5. [x] **Wire** `agent::runner` to registry. ✅
6. [x] Write **unit tests** for `CaretParser`. ✅ **COMPLETED**
7. [ ] Document in `README.md`. ⏳ **TODO**

### 🎉 **Status: Caret Syntax Implementation Complete!**

**Implemented in last Session:**
- ✅ **ToolSyntax::Caret** enum variant added with CLI support (`--tool-syntax caret`)
- ✅ **CaretParser** implementation with full regex-based parsing
- ✅ **CaretStreamProcessor** for real-time UI streaming support
- ✅ **Caret-specific tool documentation generation** (v1 syntax format)
- ✅ **Parser trait extended** with `generate_tool_documentation()` method
- ✅ **Comprehensive test coverage** (7 parser tests + 3 stream processor tests)
- ✅ **All existing tests still passing** (145/145 tests successful)

**Caret Syntax v1 Features Implemented:**
- 🔧 **Tool invocation**: `^^^tool_name` ... `^^^`
- 🔧 **Single-line parameters**: `key: value`
- 🔧 **Multi-line parameters**: `paramname ---` ... `--- paramname`
- 🔧 **Array parameters**: `key: [element1\
element2\
]`
- 🔧 **Multiple multi-line parameters** supported in same tool
- 🔧 **No indentation required** (error-resistant for LLMs)
- 🔧 **Token-efficient**: ~8 tokens vs ~20 for XML

**Architecture Improvements:**
- 🏗️ **Enhanced Parser Trait** - now includes documentation generation
- 🏗️ **Syntax-Specific Documentation** - each parser generates appropriate format
- 🏗️ **Clean Separation** - XML, JSON, and Caret parsers fully isolated
- 🏗️ **Extensible Design** - easy to add new syntaxes in future

**Test Results:**
- ✅ **Parse Tests**: 7/7 passing (simple, multiline, arrays, error handling)
- ✅ **Stream Tests**: 3/3 passing (simple, multiline, message extraction)
- ✅ **Integration**: All 145 existing tests still pass
- ✅ **CLI**: `--tool-syntax caret` option available and functional

## TODOs and Future Improvements

### 🔄 **Immediate TODOs**
1. **Documentation**: Update `README.md` with caret syntax examples and usage guide
2. **System Message**: Consider creating a dedicated caret-specific system message template
3. **Error Messages**: Improve caret parsing error messages for better user feedback

### 🚀 **Future Enhancements**
1. **Advanced Array Syntax**: Consider supporting nested objects in arrays
2. **Parameter Validation**: Add schema-based validation for caret parameters
3. **Syntax Highlighting**: Add caret syntax highlighting for development tools
4. **Performance**: Optimize regex patterns for large tool invocations
5. **Debugging**: Add debug mode for caret parsing with detailed trace output

### ⚠️ **Known Limitations**
1. **Simplified Array Parsing**: Currently arrays are converted to simple JSON format, may not handle complex nested structures perfectly
2. **Message Content Extraction**: In `CaretStreamProcessor`, structured message content extraction is simplified (returns early for structured content)
3. **Unused Code**: Some helper functions like `finalize_buffer()` are implemented but not actively used in streaming
4. **Tool State**: The `name` field in `ToolState` is stored but not used (could be removed for optimization)

### 🔧 **Code Quality Items**
1. **Remove unused imports**: `assert_fragments_match` in test files
2. **Clean up warnings**: Address dead code warnings for unused helper methods
3. **Consolidate**: `DocumentationSyntax` enum was added but not needed after trait-based approach
4. **Optimize**: Some regex compilation could be cached at module level for performance

---

## 8 Appendix — Quick Examples

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

## Konkrete Änderungsstellen im Code

Nach Analyse des bestehenden Codes sind folgende spezifische Änderungen erforderlich:

### Core-Typen erweitern

**Datei:** `crates/code_assistant/src/types.rs:230-235`
- `ToolMode` enum um `Caret` Variante erweitern
- Eventuell umbenennen zu `ToolSyntax` für bessere Klarheit
- `ValueEnum` Implementation entsprechend anpassen

### Agent Runner anpassen

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

### Tool Parsing erweitern

**Datei:** `crates/code_assistant/src/tools/parse.rs`

**Neue Funktionen hinzufügen:**
- `parse_caret_tool_invocations()` - analog zu `parse_xml_tool_invocations()`
- `parse_caret_tool_invocations_with_truncation()` - analog zu bestehender XML-Funktion
- `find_first_caret_tool_end_and_truncate()` - analog zu `find_first_tool_end_and_truncate()`

**Bestehende Funktionen anpassen:**
- `parse_and_truncate_llm_response()` in `runner.rs` muss auf Parser-Registry umgestellt werden

### Streaming Processors erweitern

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

### Neue Dateien erstellen

**Neue Dateien:**
- `crates/code_assistant/src/ui/streaming/caret_processor.rs`
- `crates/code_assistant/src/ui/streaming/caret_processor_tests.rs`
- `crates/code_assistant/src/tools/parser_registry.rs`

### CLI Integration

**Datei:** `crates/code_assistant/src/main.rs` (vermutlich)
- CLI-Argument für `--tool-syntax` erweitern um `caret` Option
- Config-Parsing entsprechend anpassen

### Systemprompte anpassen

**Datei:** `crates/code_assistant/resources/system_message_tools.md`
- Dokumentation für Caret-Syntax hinzufügen
- Beispiele für beide Syntaxformen bereitstellen

### Konkrete Parser-Implementierung

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

### Testing-Infrastruktur

**Bestehende Test-Patterns erweitern:**
- `crates/code_assistant/src/ui/streaming/xml_processor_tests.rs` als Template
- `crates/code_assistant/src/ui/streaming/json_processor_tests.rs` als Template
- `crates/code_assistant/src/tools/tests.rs` erweitern

### Backward Compatibility

**Wichtige Kompatibilitäts-Überlegungen:**
- Bestehende Prompts und Konfigurationen müssen weiterhin funktionieren
- Default bleibt XML-Syntax
- Migrations-Pfad für bestehende Sessions berücksichtigen
- Feature-Flag oder graduelle Einführung erwägen

### Zusätzliche Fundstellen

**Import-Statements überprüfen:**
- Alle Module die `ToolMode` importieren müssen entsprechend angepasst werden
- Besonders `use super::ToolMode;` Statements in verschiedenen Dateien

**System Message Templating:**
- `crates/code_assistant/src/agent/runner.rs:get_system_prompt()` - Auswahl des korrekten System-Prompts basierend auf Tool-Syntax
- Template-Ersetzung `{{tools}}` muss für alle Syntax-Modi funktionieren

**Error Handling:**
- `crates/code_assistant/src/agent/runner.rs:format_error_for_user()` - Error-Messages müssen syntax-spezifisch sein
- Tool-Result-Konvertierung in verschiedenen Modi
