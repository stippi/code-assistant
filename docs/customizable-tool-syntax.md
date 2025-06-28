
# Making Tool Blocks Pluggable – Towards a “Triple-Caret” Syntax

> **Status** Draft • 2024-06-XX
> **Authors** `assistant` ↔ `architects@code-assistant`

---

## 1 Motivation

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

## 2 Existing Architecture (quick recap)

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

## 3 Triple-Caret Block — Specification (v0)

````text
^^^write_file
project: code-assistant
path: src/lib.rs
---
content: |
  //! hello
  fn main() {}
^^^
````

* Leading & trailing fence: `^^^` at column 0.
* Opening fence MUST include the *tool name* after the fence.
* Header is RFC 822 / YAML-like `key: value` list until the line `---`.
* Everything after `---` is assigned to `content` **unless** the header already supplied `content`.
* Exactly one tool block per assistant message (maintains current safety property).

This format:

* costs 6 tokens for the wrapper vs ~20 with XML,
* can be parsed with a **single pass regex** (`^\\^\\^([a-zA-Z0-9_]+)$` etc.).
* is human-friendly (copy/paste into editors).

---

## 4 Extensibility Design

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

Array parameters defined in the canonical JSON spec can be expressed in the
fence header in two interchangeable ways:

```text
^^^read_files
paths:
  - src/main.rs
  - Cargo.toml
^^^

# or – repeating the key

^^^read_files
path: src/main.rs
path: Cargo.toml
^^^
```

The caret parser merges duplicate keys or YAML lists into a single
`serde_json::Value::Array` so the produced `ToolRequest::input` matches the
existing schema.  Supplying a single scalar value is still supported for
back-compatibility.

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

## 5 Migration & Compatibility

* Default remains **XML** to avoid breaking any existing prompt templates.
* Native JSON path unchanged.
* Caret mode is **opt-in** via CLI flag or environment variable:
  `--tool-syntax caret` or in config file.

---

## 6 Implementation Steps

1. [ ] **Extract** common behaviours from `xml_processor.rs` / `json_processor.rs` into the trait.
2. [ ] **Move** XML & JSON processors behind `XmlParser` / `JsonParser` that implement the trait.
3. [ ] **Add** `CaretParser`.
4. [ ] **Update** `ToolMode` → `ToolSyntax`; adjust CLI flag parsing.
5. [ ] **Wire** `agent::runner` to registry.
6. [ ] Write **unit tests** analogous to `xml_processor_tests.rs` & `json_processor_tests.rs`.
7. [ ] Document in `README.md`.

---

## 7 Appendix — Quick Examples

### read_files

````text
^^^read_files
project: my_proj
path: src/main.rs
^^^
````

### replace_in_file (multi-line)

````text
^^^replace_in_file
project: cool_proj
path: src/lib.rs
---
diff: |
  <<<<<<< SEARCH
  old()
  =======
  new()
  >>>>>>> REPLACE
^^^
````

### Arbitrary content field

````text
^^^write_file
project: notes
path: design.md
---
content: |
  # Title
  Multiline
  body.
^^^
````

---

**Result** – we keep the single-tool-per-turn safety contract while allowing any number of syntaxes to coexist, decoupled from the core agent loop.
