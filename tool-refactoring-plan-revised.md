# Tool Refactoring Plan (Revised)

## Current Architecture Analysis

The current tools implementation in the code-assistant project is located in `crates/code_assistant/src/tools/`. It follows a pattern where:

1. Tools are defined as enum variants in `types.rs` (`Tool` enum)
2. Tool results are also defined as enum variants in `types.rs` (`ToolResult` enum)
3. Tool definitions (descriptions, parameters) are statically defined in `definitions.rs`
4. Tool execution logic is centralized in the `ToolExecutor` in `executor.rs`
5. Parsing of tool invocations happens in `parse.rs`
6. Results processing happens in `result.rs` and `handlers.rs`

This architecture has several limitations:

- Adding a new tool requires modifying multiple files (at minimum `types.rs` and `definitions.rs`)
- Tool definitions, implementations, and results handling are spread across different files
- There's no centralized registration mechanism for tools
- The XML tool descriptions for system messages are statically defined
- Detecting redundant tool output (e.g., same file loaded multiple times) is not built-in

## Target Architecture

### Core Principles

1. **Trait-based Tool Definition**: Replace Tool enum with a trait that each tool implements
2. **Modular Tool Organization**: Each tool lives in its own module with all related code
3. **Automatic Registration**: Tools register themselves in a central registry
4. **Dynamic Description Generation**: Tool descriptions are dynamically generated from markdown files
5. **Unified Result Handling**: Clear separation between tool execution and result rendering
6. **Deduplication**: Built-in support for detecting and avoiding redundant output
7. **Self-contained Output Types**: Each tool defines its own output type without relying on a shared enum

### Key Components

#### 1. ToolSpec for Tool Definition

```rust
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters_schema: serde_json::Value,
    pub annotations: Option<serde_json::Value>, // For special LLM instructions
}
```

This encapsulates all the static metadata about a tool, making tool definitions self-contained and consistent.

#### 2. Tool Trait for Execution Logic

```rust
#[async_trait::async_trait]
pub trait Tool: Send + Sync + 'static {
    // Associated types for input and output
    type Input: DeserializeOwned + Send;
    type Output: Render + Send + Sync;

    // Static metadata
    fn spec(&self) -> &'static ToolSpec;

    // Main execution method
    async fn execute(
        &self,
        context: &mut ToolContext,
        input: Self::Input
    ) -> Result<Self::Output>;
}
```

This trait provides the interface that concrete tools must implement. Note the use of associated types for input and output, which provides type safety without the complexity of generic parameters.

#### 3. Render Trait for Output Formatting

```rust
pub trait Render: Send + Sync + 'static {
    // Generate a short status message for display in action history
    fn status(&self) -> String;

    // Format the detailed output, with awareness of other tool results
    // The resources_tracker helps detect and handle redundant output
    fn render(&self, resources_tracker: &mut ResourcesTracker) -> String;
}
```

This trait handles the formatting of tool outputs, decoupling the execution logic from the presentation concerns.

#### 4. ResourcesTracker for Deduplication

```rust
/// Tracks resources that have been included in tool outputs to prevent redundant display
pub struct ResourcesTracker {
    /// Set of already rendered resource identifiers
    rendered_resources: HashSet<String>,
}

impl ResourcesTracker {
    pub fn new() -> Self {
        Self {
            rendered_resources: HashSet::new(),
        }
    }

    /// Check if a resource has already been rendered
    pub fn is_rendered(&self, resource_id: &str) -> bool {
        self.rendered_resources.contains(resource_id)
    }

    /// Mark a resource as rendered
    pub fn mark_rendered(&mut self, resource_id: String) {
        self.rendered_resources.insert(resource_id);
    }
}
```

The ResourcesTracker helps prevent showing the same resource (like a file's content) multiple times in the output. Tools generate unique IDs for their resources, and the tracker keeps track of what has already been displayed.

#### 5. DynTool for Type Erasure

```rust
#[async_trait::async_trait]
pub trait DynTool: Send + Sync + 'static {
    fn spec(&self) -> &'static ToolSpec;

    async fn invoke(
        &self,
        context: &mut ToolContext,
        params: serde_json::Value
    ) -> Result<Box<dyn AnyOutput>>;
}

pub trait AnyOutput: Send + Sync {
    fn as_render(&self) -> &dyn Render;
}

// Automatically implemented for all Tool outputs
impl<T: Render + Send + Sync + 'static> AnyOutput for T {
    fn as_render(&self) -> &dyn Render {
        self
    }
}
```

The DynTool trait provides type erasure for tools, allowing them to be stored in a collection. The AnyOutput trait allows for type-safe access to rendering functionality without downcasting.

#### 6. Tool Registry

```rust
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn DynTool>>,
}

impl ToolRegistry {
    pub fn global() -> &'static Self {
        // Singleton instance of the registry
        static INSTANCE: OnceCell<ToolRegistry> = OnceCell::new();
        INSTANCE.get_or_init(|| {
            let mut registry = ToolRegistry::new();
            registry.register_default_tools();
            registry
        })
    }

    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: Box<dyn DynTool>) {
        self.tools.insert(tool.spec().name.to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Box<dyn DynTool>> {
        self.tools.get(name)
    }

    pub fn all(&self) -> Vec<&Box<dyn DynTool>> {
        self.tools.values().collect()
    }

    pub fn get_tool_definitions(&self) -> Vec<AnnotatedToolDefinition> {
        self.tools
            .values()
            .map(|tool| AnnotatedToolDefinition {
                name: tool.spec().name.to_string(),
                description: tool.spec().description.to_string(),
                parameters: tool.spec().parameters_schema.clone(),
                annotations: tool.spec().annotations.clone(),
            })
            .collect()
    }

    fn register_default_tools(&mut self) {
        // Register all the standard tools
        self.register(Box::new(ReadFilesTool));
        self.register(Box::new(ListFilesTool));
        // ... register other tools
    }
}
```

The ToolRegistry provides a central point for tool registration and discovery.

#### 7. Blanket Implementation for DynTool

```rust
#[async_trait::async_trait]
impl<T> DynTool for T
where
    T: Tool,
    T::Input: DeserializeOwned,
    T::Output: Render + Send + Sync + 'static,
{
    fn spec(&self) -> &'static ToolSpec {
        Tool::spec(self)
    }

    async fn invoke(
        &self,
        context: &mut ToolContext,
        params: serde_json::Value
    ) -> Result<Box<dyn AnyOutput>> {
        // Deserialize input
        let input: T::Input = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Failed to parse parameters: {}", e))?;

        // Execute the tool
        let output = self.execute(context, input).await?;

        // Box the output as AnyOutput
        Ok(Box::new(output) as Box<dyn AnyOutput>)
    }
}
```

This blanket implementation allows any concrete Tool implementation to be used where a DynTool is expected, simplifying the creation of new tools.

### Example Tool Implementation

Each tool would have its own folder under `src/tools/impls/` with the following structure:

```
src/tools/impls/
  read_files/
    mod.rs         # Tool implementation
    description.md # Tool description embedded in the binary
    tests.rs       # Tests for this tool
```

Example implementation:

```rust
// src/tools/impls/read_files/mod.rs
use crate::tools::core::{Tool, ToolSpec, Render, ToolContext, ResourcesTracker};
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

// ToolSpec definition for this tool
static READ_FILES_SPEC: ToolSpec = ToolSpec {
    name: "read_files",
    description: include_str!("description.md"),
    parameters_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "project": {
                "type": "string",
                "description": "Name of the project containing the files"
            },
            "paths": {
                "type": "array",
                "description": "Paths to the files relative to the project root directory",
                "items": {
                    "type": "string"
                }
            }
        },
        "required": ["project", "paths"]
    }),
    annotations: None,
};

// Input type
#[derive(Deserialize)]
pub struct ReadFilesInput {
    pub project: String,
    pub paths: Vec<String>,
}

// Output type
pub struct ReadFilesOutput {
    pub project: String,
    pub loaded_files: HashMap<PathBuf, String>,
    pub failed_files: Vec<(PathBuf, String)>,
}

// Render implementation for output
impl Render for ReadFilesOutput {
    fn status(&self) -> String {
        if self.failed_files.is_empty() {
            format!("Successfully loaded {} file(s)", self.loaded_files.len())
        } else {
            format!(
                "Loaded {} file(s), failed to load {} file(s)",
                self.loaded_files.len(),
                self.failed_files.len()
            )
        }
    }

    fn render(&self, tracker: &mut ResourcesTracker) -> String {
        let mut formatted = String::new();

        // Handle failed files first
        for (path, error) in &self.failed_files {
            formatted.push_str(&format!(
                "Failed to load '{}' in project '{}': {}\n",
                path.display(),
                self.project,
                error
            ));
        }

        // Format loaded files, checking for redundancy
        if !self.loaded_files.is_empty() {
            formatted.push_str("Successfully loaded the following file(s):\n");

            for (path, content) in &self.loaded_files {
                // Generate a unique resource ID for this file with content hash
                let content_hash = format!("{:x}", md5::compute(content));
                let resource_id = format!("file:{}:{}:{}", self.project, path.display(), content_hash);

                if !tracker.is_rendered(&resource_id) {
                    // This file hasn't been rendered yet
                    formatted.push_str(&format!(
                        ">>>>> FILE: {}\n{}\n<<<<< END FILE\n",
                        path.display(),
                        content
                    ));

                    // Mark as rendered
                    tracker.mark_rendered(resource_id);
                } else {
                    // This file has already been rendered
                    formatted.push_str(&format!(
                        ">>>>> FILE: {} (content shown in another tool invocation)\n<<<<< END FILE\n",
                        path.display()
                    ));
                }
            }
        }

        formatted
    }
}

// The actual tool
pub struct ReadFilesTool;

#[async_trait::async_trait]
impl Tool for ReadFilesTool {
    type Input = ReadFilesInput;
    type Output = ReadFilesOutput;

    fn spec(&self) -> &'static ToolSpec {
        &READ_FILES_SPEC
    }

    async fn execute(
        &self,
        context: &mut ToolContext,
        input: Self::Input
    ) -> Result<Self::Output> {
        let project_manager = context.project_manager;
        let mut loaded_files = HashMap::new();
        let mut failed_files = Vec::new();

        // Implementation details...

        Ok(ReadFilesOutput {
            project: input.project,
            loaded_files,
            failed_files,
        })
    }
}
```

## Implementation Strategy

Refactoring a significant piece of the system will require a careful, step-by-step approach:

### Addressing Naming Conflicts

To avoid naming conflicts during the transition phase, we'll use the following structure:

```
src/tools/
  core/          - Core traits and types for the new tool system
    mod.rs       - Exports all core types
    tool.rs      - Tool trait definition
    spec.rs      - ToolSpec structure
    render.rs    - Render trait and ResourcesTracker
    dyn_tool.rs  - DynTool trait and AnyOutput
    registry.rs  - ToolRegistry implementation
  impls/         - Individual tool implementations
    read_files/  - Implementation of read_files tool
    list_files/  - Implementation of list_files tool
    ...
  adapter.rs     - Adapter between old and new systems during transition
  registry.rs    - Main entry point that re-exports from core
  mod.rs         - Main module that controls what's exposed
```

This structure allows us to keep both the old `Tool` enum and the new `Tool` trait in the codebase simultaneously.

### Phase 1: Core Traits and Registry Implementation

1. **Create Tool and ToolSpec**: Define the core traits in `tools/core/`
2. **Implement Render and ResourcesTracker**: Create the rendering system
3. **Implement DynTool and AnyOutput**: Create the type erasure system
4. **Implement ToolRegistry**: Create the central registry for tools

### Phase 2: First Tool Migration

1. **Choose a Simple Tool**: Begin with a simple tool like `list_projects`
2. **Create Directory Structure**: Set up the directory structure for this tool
3. **Implement Tool Trait**: Migrate the tool to implement the Tool trait
4. **Register the Tool**: Register the tool with the registry
5. **Test Compatibility**: Ensure the tool works with both old and new systems

### Phase 3: Adaptation Layer

1. **Create Adapter**: Create an adapter that maps between old enum-based tools and new trait-based tools
   - Create a method to convert `crate::types::Tool` to tool invocations via the registry
   - Create a method to convert tool results from the new system to `crate::types::ToolResult`
   - Implement a wrapper that makes the new tools compatible with existing handlers

2. **Update Executor**: Modify the `ToolExecutor` to use the registry and adapters
   ```rust
   // Example adapter method in tools/adapter.rs
   pub async fn execute_with_new_system(
       tool: &crate::types::Tool,
       context: &ToolContext,
   ) -> Result<crate::types::ToolResult> {
       match tool {
           crate::types::Tool::ReadFiles { project, paths } => {
               let registry = ToolRegistry::global();
               if let Some(read_files_tool) = registry.get("read_files") {
                   // Convert parameters to JSON
                   let params = serde_json::json!({
                       "project": project,
                       "paths": paths,
                   });

                   // Execute the tool
                   let result = read_files_tool.invoke(context, params).await?;

                   // Convert the result back to the old format
                   convert_to_legacy_result(result)
               } else {
                   Err(anyhow::anyhow!("read_files tool not found in registry"))
               }
           },
           // Other tool mappings...
       }
   }
   ```

3. **Tool Definition Generation**: Generate tool descriptions for system messages dynamically
   - Create a function that generates XML tool definitions from the registry
   - Ensure compatibility with existing parsing code
   - Add tests to verify the generated XML matches expected format

### Phase 4: Gradual Migration

1. **Migrate Tools Incrementally**: Move one tool at a time to the new system
2. **Test Each Migration**: Thoroughly test each tool after migration
3. **Update Tool Handlers**: Adapt handlers to work with the new output format system

### Phase 5: Complete Transition

1. **Remove Old Code**: Once all tools are migrated, remove the old enum-based code
2. **Update Documentation**: Update all documentation to reflect the new architecture
3. **Performance Optimization**: Profile and optimize any performance bottlenecks

## Conclusion

This refactoring plan provides a clear path to transform the current tools system into a more maintainable, extensible architecture. By following a modular, trait-based approach, we can simplify the addition of new tools while improving code organization and reducing duplication. The incremental migration strategy ensures minimal disruption while gradually adopting the new design.

The proposed architecture addresses the key goals:
1. Centralized tool registry for discovery and extension
2. Self-contained tool implementation in dedicated folders
3. Automatic embedding of tool descriptions from markdown files
4. Intelligent handling of redundant output via ResourcesTracker
5. Simplified process for adding new tools

By following this plan, we can maintain compatibility during the transition while progressively moving to a more maintainable and extensible architecture.
