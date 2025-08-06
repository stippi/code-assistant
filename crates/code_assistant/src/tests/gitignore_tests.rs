use std::fs;
use std::path::PathBuf;

use crate::explorer::Explorer;
use crate::types::CodeExplorer;
use anyhow::Result;
use tempfile::TempDir;

/// Sets up a test directory with a .gitignore file and various test files
/// Returns a tuple with (TempDir, Explorer, visible_file, ignored_file)
fn setup_gitignore_test() -> Result<(TempDir, Explorer, PathBuf, PathBuf)> {
    // Create a temporary directory for our test
    let temp_dir = TempDir::new()?;
    let root_path = temp_dir.path();

    // Create a .gitignore file that ignores specific patterns
    fs::write(
        root_path.join(".gitignore"),
        "# Comment line\n*.ignored\nignored_dir/\n",
    )?;

    // Create a .git subdirectory which enables .gitignore support in the `ignore` crate
    fs::create_dir(root_path.join(".git"))?;

    // Create visible files
    let visible_file = root_path.join("visible.txt");
    fs::write(&visible_file, "This file should be visible")?;

    // Create a subdirectory with visible and ignored files
    fs::create_dir(root_path.join("subdir"))?;
    fs::write(
        root_path.join("subdir/subdir_visible.txt"),
        "This file in subdirectory should be visible",
    )?;

    // Create ignored files
    let ignored_file = root_path.join("secret.ignored");
    fs::write(&ignored_file, "This file should be ignored")?;

    // Create an ignored directory
    fs::create_dir(root_path.join("ignored_dir"))?;
    fs::write(
        root_path.join("ignored_dir/never_seen.txt"),
        "This file should never be visible",
    )?;

    // Create the Explorer instance
    let explorer = Explorer::new(root_path.to_path_buf());

    Ok((temp_dir, explorer, visible_file, ignored_file))
}

#[test]
fn test_list_files_respects_gitignore() -> Result<()> {
    let (temp_dir, mut explorer, _, _) = setup_gitignore_test()?;
    let root_path = temp_dir.path();

    // List files in the root directory
    let result = explorer.list_files(&root_path.to_path_buf(), None)?;

    // Convert the result to a string for easier inspection
    let listed_files = result.to_string();

    // Print for debugging
    println!("Listed files: {listed_files}");

    // Verify visible files are included
    assert!(listed_files.contains("visible.txt"));
    assert!(listed_files.contains("subdir"));

    // Verify ignored files are NOT included
    assert!(
        !listed_files.contains("secret.ignored"),
        "Ignored file was incorrectly listed"
    );
    assert!(
        !listed_files.contains("ignored_dir"),
        "Ignored directory was incorrectly listed"
    );
    assert!(
        !listed_files.contains("never_seen.txt"),
        "File in ignored directory was incorrectly listed"
    );

    // Also list files in the subdirectory to make sure that works
    let subdir_path = root_path.join("subdir");
    let subdir_result = explorer.list_files(&subdir_path, None)?;
    let subdir_listed = subdir_result.to_string();

    // Verify subdirectory content is correct
    assert!(subdir_listed.contains("subdir_visible.txt"));

    Ok(())
}

#[test]
fn test_read_files_respects_gitignore() -> Result<()> {
    let (_temp_dir, explorer, visible_file, ignored_file) = setup_gitignore_test()?;

    // Reading a visible file should succeed
    let visible_content = explorer.read_file(&visible_file)?;
    assert_eq!(visible_content, "This file should be visible");

    // Reading an ignored file should fail
    let ignored_result = explorer.read_file(&ignored_file);
    assert!(
        ignored_result.is_err(),
        "Should not be able to read ignored files"
    );

    // The error should indicate the file is hidden by .gitignore
    let error = ignored_result.unwrap_err().to_string();
    assert!(
        error.contains("ignored") || error.contains("hidden") || error.contains("gitignore"),
        "Error message doesn't mention the file is ignored: {error}"
    );

    // Test read_file_range with line ranges
    let visible_range = explorer.read_file_range(&visible_file, Some(1), Some(1))?;

    // Check content is correct (trim needed due to line ending normalization)
    assert!(visible_range.trim_end() == "This file should be visible");

    // Reading an ignored file with line range should also fail
    let ignored_range = explorer.read_file_range(&ignored_file, Some(1), Some(1));
    assert!(
        ignored_range.is_err(),
        "Should not be able to read ignored files with line range"
    );

    Ok(())
}

#[test]
fn test_write_file_respects_gitignore() -> Result<()> {
    let (_temp_dir, explorer, visible_file, ignored_file) = setup_gitignore_test()?;

    // Writing to a visible file should succeed
    let new_content = "Updated visible content";
    let write_visible = explorer.write_file(&visible_file, &new_content.to_string(), false)?;

    // Prüfen, dass der Inhalt korrekt ist (ignoriere mögliche Zeilenumbrüche am Ende)
    assert_eq!(write_visible.trim_end(), new_content);

    // Prüfen, dass die Datei tatsächlich aktualisiert wurde
    let file_content = fs::read_to_string(&visible_file)?;
    assert_eq!(file_content.trim_end(), new_content);

    // Writing to an ignored file should fail
    let write_ignored = explorer.write_file(
        &ignored_file,
        &"Trying to update ignored file".to_string(),
        false,
    );
    assert!(write_ignored.is_err());

    // The error should indicate the file is hidden by .gitignore
    let error = write_ignored.unwrap_err().to_string();
    assert!(
        error.contains("ignored") || error.contains("hidden") || error.contains("gitignore"),
        "Error message doesn't mention the file is ignored: {error}"
    );

    // Appending to an ignored file should also fail
    let append_ignored = explorer.write_file(
        &ignored_file,
        &"Trying to append to ignored file".to_string(),
        true,
    );
    assert!(append_ignored.is_err());

    Ok(())
}

#[test]
fn test_gitignore_doesnt_affect_direct_fs_operations() -> Result<()> {
    // This test verifies that our tests are valid by confirming that
    // direct filesystem operations can still access the ignored files

    let (_temp_dir, _, _, ignored_file) = setup_gitignore_test()?;

    // We should be able to read the ignored file directly with fs functions
    let content = fs::read_to_string(&ignored_file)?;
    assert_eq!(content, "This file should be ignored");

    // We should be able to write to the ignored file directly with fs functions
    let new_content = "Updated ignored content";
    fs::write(&ignored_file, new_content)?;
    assert_eq!(fs::read_to_string(&ignored_file)?, new_content);

    Ok(())
}
