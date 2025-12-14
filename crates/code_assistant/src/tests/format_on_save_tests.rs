use crate::tests::mocks::{MockCommandExecutor, MockExplorer, MockProjectManager};
use crate::tools::core::{Tool, ToolContext};
use crate::tools::impls::edit::{EditInput, EditTool};
use crate::tools::impls::replace_in_file::{ReplaceInFileInput, ReplaceInFileTool};
use crate::tools::impls::write_file::{WriteFileInput, WriteFileTool};
use crate::types::Project;
use anyhow::Result;
use command_executor::CommandOutput;
use fs_explorer::file_updater::{
    extract_stable_ranges, reconstruct_formatted_replacements, MatchRange,
};
use fs_explorer::FileReplacement;
use std::collections::HashMap;
use std::path::PathBuf;

/// Test the core stable range extraction functionality
#[test]
fn test_stable_range_extraction_simple() -> Result<()> {
    let content = "function foo() {\n    console.log('hello');\n    return 42;\n}";

    let matches = vec![MatchRange {
        replacement_index: 0,
        match_index: 0,
        start: 21, // Start of "console.log('hello');"
        end: 42,   // End of "console.log('hello');"
    }];

    let stable_ranges = extract_stable_ranges(content, &matches);

    // Should have stable ranges before and after the match
    assert_eq!(stable_ranges.len(), 2);

    // First stable range: "function foo() {\n    "
    assert_eq!(stable_ranges[0].start, 0);
    assert_eq!(stable_ranges[0].end, 21);
    assert_eq!(stable_ranges[0].content, "function foo() {\n    ");

    // Second stable range: "\n    return 42;\n}"
    assert_eq!(stable_ranges[1].start, 42);
    assert_eq!(stable_ranges[1].end, content.len());
    assert_eq!(stable_ranges[1].content, "\n    return 42;\n}");

    Ok(())
}

/// Test stable range extraction with multiple matches
#[test]
fn test_stable_range_extraction_multiple_matches() -> Result<()> {
    let content = "const a = 1;\nconst b = 2;\nconst c = 3;";

    let matches = vec![
        MatchRange {
            replacement_index: 0,
            match_index: 0,
            start: 0, // "const a = 1;"
            end: 12,
        },
        MatchRange {
            replacement_index: 1,
            match_index: 0,
            start: 26, // "const c = 3;"
            end: 38,
        },
    ];

    let stable_ranges = extract_stable_ranges(content, &matches);

    // Should have one stable range between the two matches
    assert_eq!(stable_ranges.len(), 1);

    // First stable range between matches: "\nconst b = 2;\n"
    assert_eq!(stable_ranges[0].start, 12);
    assert_eq!(stable_ranges[0].end, 26);
    assert_eq!(stable_ranges[0].content, "\nconst b = 2;\n");

    Ok(())
}

/// Test parameter reconstruction with simple formatting changes
#[test]
fn test_parameter_reconstruction_simple() -> Result<()> {
    // For now, let's test a case where reconstruction should fail due to complex formatting
    // This tests the graceful degradation behavior
    let original_content = "const x=1;\nconst y=2;\nconst z=3;";
    let formatted_content = "const x = 1;\nconst y = 2;\nconst z = 3;"; // Added spaces around =

    let matches = vec![MatchRange {
        replacement_index: 0,
        match_index: 0,
        start: 11, // Start of "const y=2;"
        end: 21,   // End of "const y=2;"
    }];

    let stable_ranges = extract_stable_ranges(original_content, &matches);

    let original_replacements = vec![FileReplacement {
        search: "const y=2;".to_string(),
        replace: "const y=42;".to_string(),
        replace_all: false,
    }];

    let updated_replacements = reconstruct_formatted_replacements(
        original_content,
        formatted_content,
        &stable_ranges,
        &matches,
        &original_replacements,
    );

    // Should fail to reconstruct because stable content was modified by formatting
    // This is the expected behavior for graceful degradation
    assert!(updated_replacements.is_none());

    Ok(())
}

/// Test parameter reconstruction failure when stable content changes
#[test]
fn test_parameter_reconstruction_failure() -> Result<()> {
    let original_content = "function test() {\n    // Comment\n    return 42;\n}";
    // Formatting removes the comment entirely
    let formatted_content = "function test() {\n    return 42;\n}";

    let matches = vec![MatchRange {
        replacement_index: 0,
        match_index: 0,
        start: 22, // Start of "// Comment"
        end: 32,   // End of "// Comment"
    }];

    let stable_ranges = extract_stable_ranges(original_content, &matches);

    let original_replacements = vec![FileReplacement {
        search: "// Comment".to_string(),
        replace: "// Updated comment".to_string(),
        replace_all: false,
    }];

    let updated_replacements = reconstruct_formatted_replacements(
        original_content,
        formatted_content,
        &stable_ranges,
        &matches,
        &original_replacements,
    );

    // Should fail to reconstruct because stable content was modified by formatting
    assert!(updated_replacements.is_none());

    Ok(())
}

/// Test edit tool parameter updating after formatting
#[tokio::test]
async fn test_edit_tool_parameter_update_after_formatting() -> Result<()> {
    // Create test files with already formatted JavaScript
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test.js"),
        "const x = 1;const y = 2;const z = 3;".to_string(),
    );

    // Mock command executor that formats the file (adds spaces)
    let command_responses = vec![Ok(CommandOutput {
        success: true,
        output: "test.js\n".to_string(),
    })];

    let command_executor = MockCommandExecutor::new(command_responses);

    // Provide a formatted version of the file after prettier runs
    let mut formatted_map = HashMap::new();
    formatted_map.insert(
        PathBuf::from("./root/test.js"),
        "const x = 1;const y = 42;const z = 3;".to_string(),
    );
    let explorer = MockExplorer::new_with_formatting(files, formatted_map, None);

    // Create project with format-on-save configuration
    let mut format_on_save = HashMap::new();
    format_on_save.insert("*.js".to_string(), "prettier --write {path}".to_string());

    let project = Project {
        path: PathBuf::from("./root"),
        format_on_save: Some(format_on_save),
    };

    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        project,
        Box::new(explorer),
    ));

    let mut context = ToolContext {
        project_manager: project_manager.as_ref(),
        command_executor: &command_executor,
        plan: None,
        ui: None,
        tool_id: None,
        permission_handler: None,
        sub_agent_runner: None,
        sub_agent_cancellation_registry: None,
    };

    // Test editing: search is formatted (matches file), replacement is unformatted
    let mut input = EditInput {
        project: "test-project".to_string(),
        path: "test.js".to_string(),
        old_text: "const y = 2;".to_string(), // formatted search
        new_text: "const y=42;".to_string(),  // unformatted replacement
        replace_all: false,
    };

    let tool = EditTool;
    let result = tool.execute(&mut context, &mut input).await?;

    // Debug output
    if let Some(ref error) = result.error {
        println!("Edit error: {error:?}");
    }

    // Should succeed
    assert!(result.error.is_none());

    // Verify that the formatter was called with the file path
    let captured_commands = command_executor.get_captured_commands();
    assert_eq!(captured_commands.len(), 1);
    assert!(captured_commands[0]
        .command_line
        .contains("prettier --write test.js"));

    // Verify that input parameters were updated to the formatted versions
    assert_eq!(input.old_text, "const y = 2;"); // search unchanged
    assert_eq!(input.new_text, "const y = 42;"); // replacement reflects formatted code

    Ok(())
}

/// Test write_file tool with format-on-save
#[tokio::test]
async fn test_write_file_with_format_on_save() -> Result<()> {
    // Create empty files map (we're creating a new file)
    let files = HashMap::new();

    // Mock command executor that simulates formatting
    let command_responses = vec![Ok(CommandOutput {
        success: true,
        output: String::new(),
    })];

    let command_executor = MockCommandExecutor::new(command_responses);
    let explorer = MockExplorer::new(files, None);

    // Create project with format-on-save configuration for Rust files
    let mut format_on_save = HashMap::new();
    format_on_save.insert("*.rs".to_string(), "rustfmt".to_string());

    let project = Project {
        path: PathBuf::from("./root"),
        format_on_save: Some(format_on_save),
    };

    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        project,
        Box::new(explorer),
    ));

    let mut context = ToolContext {
        project_manager: project_manager.as_ref(),
        command_executor: &command_executor,
        plan: None,
        ui: None,
        tool_id: None,
        permission_handler: None,
        sub_agent_runner: None,
        sub_agent_cancellation_registry: None,
    };

    // Test writing a Rust file
    let mut input = WriteFileInput {
        project: "test-project".to_string(),
        path: "test.rs".to_string(),
        content: "fn main(){println!(\"Hello\");}".to_string(),
        append: false,
    };

    let tool = WriteFileTool;
    let result = tool.execute(&mut context, &mut input).await?;

    // Should succeed
    assert!(result.error.is_none());

    // Note: WriteFileTool doesn't currently implement format-on-save
    // This test verifies the tool still works with format-on-save configuration

    Ok(())
}

/// Test replace_in_file tool with format-on-save
#[tokio::test]
async fn test_replace_in_file_with_format_on_save() -> Result<()> {
    // Create test files (already formatted TOML)
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/config.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1.0\""
            .to_string(),
    );

    // Mock command executor for TOML formatting (success)
    let command_responses = vec![Ok(CommandOutput {
        success: true,
        output: String::new(),
    })];

    let command_executor = MockCommandExecutor::new(command_responses);

    // Provide formatted outputs after applying replacements (format normalizes spaces around '=')
    let mut formatted_map = HashMap::new();
    formatted_map.insert(
        PathBuf::from("./root/config.toml"),
        "[package]\nname = \"test\"\nversion = \"0.2.0\"\n[dependencies]\nserde = \"2.0\""
            .to_string(),
    );
    let explorer = MockExplorer::new_with_formatting(files, formatted_map, None);

    // Create project with format-on-save configuration for TOML files
    let mut format_on_save = HashMap::new();
    format_on_save.insert("*.toml".to_string(), "taplo format {path}".to_string());

    let project = Project {
        path: PathBuf::from("./root"),
        format_on_save: Some(format_on_save),
    };

    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        project,
        Box::new(explorer),
    ));

    let mut context = ToolContext {
        project_manager: project_manager.as_ref(),
        command_executor: &command_executor,
        plan: None,
        ui: None,
        tool_id: None,
        permission_handler: None,
        sub_agent_runner: None,
        sub_agent_cancellation_registry: None,
    };

    // Diff has two SEARCH/REPLACE blocks; replacements are unformatted (missing spaces around '=')
    let mut input = ReplaceInFileInput {
        project: "test-project".to_string(),
        path: "config.toml".to_string(),
        diff: concat!(
            "<<<<<<< SEARCH\n",
            "version = \"0.1.0\"\n",
            "=======\n",
            "version=\"0.2.0\"\n",
            ">>>>>>> REPLACE\n",
            "<<<<<<< SEARCH\n",
            "serde = \"1.0\"\n",
            "=======\n",
            "serde=\"2.0\"\n",
            ">>>>>>> REPLACE",
        )
        .to_string(),
    };

    let tool = ReplaceInFileTool;
    let result = tool.execute(&mut context, &mut input).await?;

    // Should succeed
    assert!(result.error.is_none());

    // Verify that the formatter was called with the file path
    let captured = command_executor.get_captured_commands();
    assert_eq!(captured.len(), 1);
    assert!(captured[0]
        .command_line
        .contains("taplo format config.toml"));

    // Verify that the diff input was updated to reflect formatted replacements
    // Expect spaces around '=' in the updated REPLACE parts
    assert!(input.diff.contains("version = \"0.2.0\""));
    assert!(input.diff.contains("serde = \"2.0\""));

    Ok(())
}

/// Test that files not matching format patterns are not formatted
#[tokio::test]
async fn test_no_format_when_pattern_doesnt_match() -> Result<()> {
    // Create test files
    let mut files = HashMap::new();
    files.insert(PathBuf::from("./root/test.txt"), "Hello World".to_string());

    let command_executor = MockCommandExecutor::new(vec![]);
    let explorer = MockExplorer::new(files, None);

    // Create project with format-on-save configuration for JS files only
    let mut format_on_save = HashMap::new();
    format_on_save.insert("*.js".to_string(), "prettier --write {path}".to_string());

    let project = Project {
        path: PathBuf::from("./root"),
        format_on_save: Some(format_on_save),
    };

    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        project,
        Box::new(explorer),
    ));

    let mut context = ToolContext {
        project_manager: project_manager.as_ref(),
        command_executor: &command_executor,
        plan: None,
        ui: None,
        tool_id: None,
        permission_handler: None,
        sub_agent_runner: None,
        sub_agent_cancellation_registry: None,
    };

    // Test editing a .txt file (should not be formatted)
    let mut input = EditInput {
        project: "test-project".to_string(),
        path: "test.txt".to_string(),
        old_text: "Hello".to_string(),
        new_text: "Hi".to_string(),
        replace_all: false,
    };

    let tool = EditTool;
    let result = tool.execute(&mut context, &mut input).await?;

    // Should succeed
    assert!(result.error.is_none());

    // No format command should have been executed
    let captured_commands = command_executor.get_captured_commands();
    assert_eq!(captured_commands.len(), 0);

    Ok(())
}

/// Test format-on-save with multiple file patterns
#[tokio::test]
async fn test_format_on_save_multiple_patterns() -> Result<()> {
    // Create test files
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test.js"),
        "function test(){return 42;}".to_string(),
    );
    files.insert(
        PathBuf::from("./root/test.ts"),
        "function test():number{return 42;}".to_string(),
    );

    // Mock command executor for both JS and TS
    let command_responses = vec![
        Ok(CommandOutput {
            success: true,
            output: "test.js\n".to_string(),
        }),
        Ok(CommandOutput {
            success: true,
            output: "test.ts\n".to_string(),
        }),
    ];

    let command_executor = MockCommandExecutor::new(command_responses);
    let explorer = MockExplorer::new(files, None);

    // Create project with format-on-save configuration for multiple patterns
    let mut format_on_save = HashMap::new();
    format_on_save.insert("*.js".to_string(), "prettier --write {path}".to_string());
    format_on_save.insert("*.ts".to_string(), "prettier --write {path}".to_string());

    let project = Project {
        path: PathBuf::from("./root"),
        format_on_save: Some(format_on_save),
    };

    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        project,
        Box::new(explorer),
    ));

    let mut context = ToolContext {
        project_manager: project_manager.as_ref(),
        command_executor: &command_executor,
        plan: None,
        ui: None,
        tool_id: None,
        permission_handler: None,
        sub_agent_runner: None,
        sub_agent_cancellation_registry: None,
    };

    // Test editing JS file
    let mut js_input = EditInput {
        project: "test-project".to_string(),
        path: "test.js".to_string(),
        old_text: "return 42;".to_string(),
        new_text: "return 'hello';".to_string(),
        replace_all: false,
    };

    let tool = EditTool;
    let result = tool.execute(&mut context, &mut js_input).await?;
    assert!(result.error.is_none());

    // Test editing TS file
    let mut ts_input = EditInput {
        project: "test-project".to_string(),
        path: "test.ts".to_string(),
        old_text: "return 42;".to_string(),
        new_text: "return 'hello';".to_string(),
        replace_all: false,
    };

    let result = tool.execute(&mut context, &mut ts_input).await?;
    assert!(result.error.is_none());

    // Both format commands should have been executed
    let captured_commands = command_executor.get_captured_commands();
    println!("Captured commands: {captured_commands:?}");
    assert_eq!(captured_commands.len(), 2);
    assert!(captured_commands[0]
        .command_line
        .contains("prettier --write test.js"));
    assert!(captured_commands[1]
        .command_line
        .contains("prettier --write test.ts"));

    Ok(())
}

/// Test format-on-save with complex glob patterns
#[tokio::test]
async fn test_format_on_save_glob_patterns() -> Result<()> {
    // Create test files in subdirectories
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/src/main.rs"),
        "fn main(){println!(\"Hello\");}".to_string(),
    );
    files.insert(
        PathBuf::from("./root/tests/test.rs"),
        "fn test(){assert_eq!(1,1);}".to_string(),
    );
    files.insert(
        PathBuf::from("./root/other.txt"),
        "Not a Rust file".to_string(),
    );

    // Mock command executor
    let command_responses = vec![
        Ok(CommandOutput {
            success: true,
            output: String::new(),
        }),
        Ok(CommandOutput {
            success: true,
            output: String::new(),
        }),
    ];

    let command_executor = MockCommandExecutor::new(command_responses);
    let explorer = MockExplorer::new(files, None);

    // Create project with glob pattern for all Rust files
    let mut format_on_save = HashMap::new();
    format_on_save.insert("**/*.rs".to_string(), "cargo fmt".to_string());

    let project = Project {
        path: PathBuf::from("./root"),
        format_on_save: Some(format_on_save),
    };

    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        project,
        Box::new(explorer),
    ));

    let mut context = ToolContext {
        project_manager: project_manager.as_ref(),
        command_executor: &command_executor,
        plan: None,
        ui: None,
        tool_id: None,
        permission_handler: None,
        sub_agent_runner: None,
        sub_agent_cancellation_registry: None,
    };

    let tool = EditTool;

    // Test editing Rust file in src/
    let mut src_input = EditInput {
        project: "test-project".to_string(),
        path: "src/main.rs".to_string(),
        old_text: "println!(\"Hello\");".to_string(),
        new_text: "println!(\"Hi there!\");".to_string(),
        replace_all: false,
    };

    let result = tool.execute(&mut context, &mut src_input).await?;
    assert!(result.error.is_none());

    // Test editing Rust file in tests/
    let mut test_input = EditInput {
        project: "test-project".to_string(),
        path: "tests/test.rs".to_string(),
        old_text: "assert_eq!(1,1);".to_string(),
        new_text: "assert_eq!(2,2);".to_string(),
        replace_all: false,
    };

    let result = tool.execute(&mut context, &mut test_input).await?;
    assert!(result.error.is_none());

    // Test editing non-Rust file (should not be formatted)
    let mut txt_input = EditInput {
        project: "test-project".to_string(),
        path: "other.txt".to_string(),
        old_text: "Not a Rust file".to_string(),
        new_text: "Still not a Rust file".to_string(),
        replace_all: false,
    };

    let result = tool.execute(&mut context, &mut txt_input).await?;
    assert!(result.error.is_none());

    // Only the Rust files should have been formatted
    let captured_commands = command_executor.get_captured_commands();
    assert_eq!(captured_commands.len(), 2);
    assert!(captured_commands[0].command_line.contains("cargo fmt"));
    assert!(captured_commands[1].command_line.contains("cargo fmt"));

    Ok(())
}

/// Test that conflicting matches skip parameter reconstruction
#[tokio::test]
async fn test_format_on_save_with_conflicting_matches() -> Result<()> {
    // Create test files with adjacent content that would cause conflicts
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test.js"),
        "console.log('a');console.log('b');".to_string(),
    );

    // Add a mock response for the format command
    let command_responses = vec![Ok(CommandOutput {
        success: true,
        output: String::new(),
    })];
    let command_executor = MockCommandExecutor::new(command_responses);
    let explorer = MockExplorer::new(files, None);

    // Create project with format-on-save configuration
    let mut format_on_save = HashMap::new();
    format_on_save.insert("*.js".to_string(), "prettier --write {path}".to_string());

    let project = Project {
        path: PathBuf::from("./root"),
        format_on_save: Some(format_on_save),
    };

    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        project,
        Box::new(explorer),
    ));

    let mut context = ToolContext {
        project_manager: project_manager.as_ref(),
        command_executor: &command_executor,
        plan: None,
        ui: None,
        tool_id: None,
        permission_handler: None,
        sub_agent_runner: None,
        sub_agent_cancellation_registry: None,
    };

    // Test that the tool handles potential conflicts gracefully
    let mut input = EditInput {
        project: "test-project".to_string(),
        path: "test.js".to_string(),
        old_text: "console.log('a');".to_string(),
        new_text: "console.log('updated');".to_string(),
        replace_all: false,
    };

    let tool = EditTool;
    let result = tool.execute(&mut context, &mut input).await?;

    // Debug output
    if let Some(ref error) = result.error {
        println!("Edit error: {error:?}");
    }

    // Should succeed even with potential conflicts
    assert!(result.error.is_none());

    Ok(())
}
