# Tool Refactoring Plan

## Current Architecture Analysis

The current tools implementation in the code-assistant project is located in `crates/code_assistant/src/tools/`. It follows a pattern where:

1. Tools are defined as enum variants in `types.rs` (`Tool` enum)
2. Tool results are also defined as enum variants in `types.rs` (`ToolResult` enum)
3. Tool definitions (descriptions, parameters) are statically defined in `definitions.rs`
4. Tool execution logic is centralized in the `ToolExecutor` in `executor.rs`
5. Parsing of tool invocations happens in `parse.rs`
6. Results processing happens in `result.rs` and `handlers.rs`

This architecture has several limitations:

- Adding a new tool requires modifying multiple files
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
// Define available modes for tools
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    McpServer,
    WorkingMemoryAgent,
    MessageHistoryAgent,
}

#[derive(Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters_schema: serde_json::Value,
    pub annotations: Option<serde_json::Value>, // For special LLM instructions
    pub supported_modes: &'static [ToolMode], // Which execution modes this tool supports
}
```

This encapsulates all the static metadata about a tool, making tool definitions self-contained and consistent.

#### 2. Tool Trait for Execution Logic

```rust
#[async_trait::async_trait]
pub trait Tool: Send + Sync + 'static {
    // Associated types for input and output
    type Input: DeserializeOwned + Send;
    type Output: Render + ToolResult + Send + Sync;

    // Tool metadata
    fn spec(&self) -> ToolSpec;

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

#### 4. ToolResult Trait for Success Status

```rust
pub trait ToolResult: Send + Sync + 'static {
    // Indicates whether the tool execution was successful
    fn is_success(&self) -> bool;
}
```

This trait provides a consistent way to determine if a tool execution was successful, separate from the rendering concerns.

#### 5. ResourcesTracker for Deduplication

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

#### 6. DynTool for Type Erasure

```rust
#[async_trait::async_trait]
pub trait DynTool: Send + Sync + 'static {
    fn spec(&self) -> ToolSpec;

    async fn invoke(
        &self,
        context: &mut ToolContext,
        params: serde_json::Value
    ) -> Result<Box<dyn AnyOutput>>;
}

pub trait AnyOutput: Send + Sync {
    fn as_render(&self) -> &dyn Render;

    fn is_success(&self) -> bool;
}

// Automatically implemented for all Tool outputs
impl<T: Render + ToolResult + Send + Sync + 'static> AnyOutput for T {
    fn as_render(&self) -> &dyn Render {
        self
    }

    fn is_success(&self) -> bool {
        ToolResult::is_success(self)
    }
}
```

The DynTool trait provides type erasure for tools, allowing them to be stored in a collection.
The AnyOutput trait allows for type-safe access to both rendering functionality and success status without downcasting.

#### 7. Tool Registry

```rust
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn DynTool>>,
}

impl ToolRegistry {
    pub fn global() -> &'static Self {
        // Singleton instance of the registry
        static INSTANCE: OnceLock<ToolRegistry> = OnceLock::new();
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

    // Get tools for a specific mode
    pub fn tools_for_mode(&self, mode: ToolMode) -> Vec<&Box<dyn DynTool>> {
        self.tools
            .values()
            .filter(|tool| {
                tool.spec().supported_modes.contains(&mode)
            })
            .collect()
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

    // Get tool definitions for a specific mode
    pub fn get_tool_definitions_for_mode(&self, mode: ToolMode) -> Vec<AnnotatedToolDefinition> {
        self.tools
            .values()
            .filter(|tool| tool.spec().supported_modes.contains(&mode))
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
        use crate::tools::impls::{ListProjectsTool, ReadFilesTool};

        self.register(Box::new(ListProjectsTool));
        self.register(Box::new(ReadFilesTool));
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
    fn spec(&self) -> ToolSpec {
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

Each tool lives in its own folder under `src/tools/impls/` with the following structure:

```
src/tools/impls/
  read_files/
    mod.rs         # Tool implementation
    description.md # Tool description embedded in the binary
    tests.rs       # Tests for this tool (if needed)
```

Example implementation:

```rust
// src/tools/impls/read_files/mod.rs
use crate::tools::core::{Render, ResourcesTracker, Tool, ToolContext, ToolMode, ToolSpec};
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

// Input type for the read_files tool
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

// Render implementation for output formatting
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

// Tool implementation
pub struct ReadFilesTool;

#[async_trait::async_trait]
impl Tool for ReadFilesTool {
    type Input = ReadFilesInput;
    type Output = ReadFilesOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
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
                        "description": "Paths to the files relative to the project root directory. Can include line ranges using 'file.txt:10-20' syntax.",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["project", "paths"]
            }),
            annotations: None,
            supported_modes: &[
                ToolMode::McpServer,
                ToolMode::WorkingMemoryAgent,
                ToolMode::MessageHistoryAgent,
            ],
        }
    }

    async fn execute(
        &self,
        context: &mut ToolContext,
        input: Self::Input
    ) -> Result<Self::Output> {
        // Implementation details...
        // ...

        Ok(ReadFilesOutput {
            project: input.project,
            loaded_files: HashMap::new(),
            failed_files: Vec::new(),
        })
    }
}
```

## Implementation Strategy

Refactoring a significant piece of the system requires a careful, step-by-step approach:

### Addressing Naming Conflicts

To avoid naming conflicts during the transition phase, we use the following structure:

```
src/tools/
  core/          - Core traits and types for the new tool system
  impls/         - Individual tool implementations
  adapter.rs     - Adapter between old and new systems during transition
  mod.rs         - Main module that controls what's exposed
```

This structure allows us to keep both the old `Tool` enum and the new `Tool` trait in the codebase simultaneously.

### Phase 1: Core Traits and Registry Implementation ✓

1. **Create Tool and ToolSpec**: Define the core traits in `tools/core/` ✓
2. **Implement Render and ResourcesTracker**: Create the rendering system ✓
3. **Implement DynTool and AnyOutput**: Create the type erasure system ✓
4. **Implement ToolRegistry**: Create the central registry for tools ✓

### Phase 2: First Tool Migration ✓

1. **Choose a Simple Tool**: Implement a simple tool like `list_projects` ✓
2. **Create Directory Structure**: Set up the directory structure for this tool ✓
3. **Implement Tool Trait**: Implement the Tool trait ✓
4. **Register the Tool**: Register the tool with the registry ✓
5. **Create Tests**: Ensure the tool works with appropriate tests ✓

### Phase 3: Adaptation Layer (In Progress)

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

3. **XML/JSON Parameter Handling**: Create a conversion function to handle parameters from XML/JSON
   - Use each tool's `parameters_schema` to understand the expected types
   - Create a conversion function that transforms `HashMap<String, Vec<String>>` to `serde_json::Value`
   - Add tests to verify parameter conversion works correctly

### Phase 4: Gradual Migration

1. **Migrate Tools Incrementally**: Move one tool at a time to the new system
2. **Test Each Migration**: Thoroughly test each tool after migration
3. **Update Tool Handlers**: Adapt handlers to work with the new output format system

### Phase 5: Complete Transition

1. **Remove Old Code**: Once all tools are migrated, remove the old enum-based code
2. **Update Documentation**: Update all documentation to reflect the new architecture
3. **Performance Optimization**: Profile and optimize any performance bottlenecks

## Agent Refactoring Plan

After refactoring the tool system to use a trait-based approach, we need to update the agent implementation to work with the new ToolRegistry and remove the WorkingMemory concept, which has not been effective for several reasons:

1. Current LLMs are not well-trained for the WorkingMemory approach
2. Maintaing detailed action history gets unwieldy with complex tool parameters
3. The MessageHistory approach has proven more effective in practice

### Key Changes

1. **Remove WorkingMemory**:
   - Remove all WorkingMemory-related functionality
   - Store necessary agent state directly in the Agent struct
   - Use MessageHistory exclusively to maintain context

2. **Simplify Agent Actions**:
   - Use ContentBlock::ToolUse directly instead of custom AgentAction
   - Remove the "Reasoning" field as it's not necessary

3. **Update parse_llm_response**:
```rust
pub(crate) fn parse_llm_response(
    response: &llm::LLMResponse,
    request_id: u64,
) -> Result<Vec<ContentBlock>> {
    // Extract all ToolUse content blocks
    // For XML parsing, convert to ContentBlock::ToolUse
    // Return vector of ContentBlock::ToolUse
}
```

4. **Update execute_action Method**:
```rust
async fn execute_action(&mut self, tool_use: &ContentBlock) -> Result<(String, bool)> {
    // Extract tool_name and params from ContentBlock::ToolUse
    // Get tool from ToolRegistry::global()
    // Create ToolContext with appropriate references
    // Invoke tool and get result
    // Use ResourcesTracker to format output
    // Return (formatted_output, is_success)
}
```

5. **Simplify State Persistence**:
   - Only store MessageHistory (which includes all tool results)
   - Update start_from_state to just restore MessageHistory
   - Regenerate tool outputs when loading state using ResourcesTracker
   - Ensure the newest tool invocation for duplicated resources is the one displayed

6. **Rename Methods for Clarity**:
   - Review all method names to ensure they match their updated functionality
   - Remove WorkingMemory-specific methods
   - Update remaining methods to use the new ToolRegistry approach

This approach will significantly simplify the agent implementation while making it more effective. By focusing on MessageHistory and using the trait-based tool system directly, we'll have a more maintainable and extensible architecture.

## Conclusion

This refactoring plan transforms the current tools system into a more maintainable, extensible architecture. By following a modular, trait-based approach, we simplify the addition of new tools while improving code organization and reducing duplication. The incremental migration strategy ensures minimal disruption while gradually adopting the new design.

The architecture addresses the key goals:
1. Centralized tool registry for discovery and extension
2. Self-contained tool implementation in dedicated folders
3. Automatic embedding of tool descriptions from markdown files
4. Intelligent handling of redundant output via ResourcesTracker
5. Simplified process for adding new tools
6. Improved agent implementation with focus on MessageHistory and removal of the problematic WorkingMemory

By following this plan, we maintain compatibility during the transition while progressively moving to a more maintainable and extensible architecture.

## Implementation Progress

- ✓ Implemented core traits and interfaces (Tool, ToolSpec, Render, ResourcesTracker, ToolResult)
- ✓ Created registry for tool discovery
- ✓ Implemented `list_projects` tool
- ✓ Implemented `read_files` tool
- ✓ Created basic adapter framework
- ✓ Added integration tests showing end-to-end flow
- ✓ Added ToolResult trait for determining success/failure of tool execution
- ✓ Implemented ToolResult for all tool output types
- ✓ Integrated success/failure information with DynTool and AnyOutput
- ⏳ Handling XML/JSON parameter conversion (in progress)
- ⏳ Integrating with existing executor
- ⏳ Migrating remaining tools
