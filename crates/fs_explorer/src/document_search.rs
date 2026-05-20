//! Document search support for search_files.
//!
//! When the `document-conversion` feature is enabled, this module walks the project
//! directory looking for supported document files (PDF, DOCX, XLSX, PPTX, ODT, RTF),
//! converts them to Markdown page-by-page using `transmutation`, and searches the
//! resulting text with the user's regex pattern. Matches are reported with context lines
//! and grouped when they are close together, mirroring the behavior of text file search.

use crate::types::DocumentMatchResult;
use regex::RegexBuilder;
use std::path::{Path, PathBuf};
use transmutation::{Converter, OutputFormat};
use walkdir::WalkDir;

/// Document extensions we search within.
const DOCUMENT_EXTENSIONS: &[&str] = &["pdf", "docx", "xlsx", "pptx", "odt", "rtf"];

/// Maximum number of documents to search (to avoid huge delays).
const MAX_DOCUMENTS_TO_SEARCH: usize = 20;

/// Maximum file size for documents to search (10 MB).
const MAX_SEARCH_DOCUMENT_SIZE: u64 = 10 * 1024 * 1024;

/// Context lines before and after a match.
const CONTEXT_LINES: usize = 2;

/// Search for `regex_pattern` within document files under `root_dir`.
///
/// If `paths` is provided, only documents under those directories are searched.
/// Returns a list of matches with page numbers, context lines, and highlight info.
pub async fn search_in_documents(
    root_dir: &Path,
    regex_pattern: &str,
    paths: Option<&[String]>,
) -> Vec<DocumentMatchResult> {
    // Build the regex (case-insensitive by default, matching search_files behavior)
    let regex = match RegexBuilder::new(regex_pattern)
        .case_insensitive(true)
        .build()
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    // Collect document files to search
    let search_roots: Vec<PathBuf> = if let Some(paths) = paths {
        paths
            .iter()
            .map(|p| root_dir.join(p))
            .filter(|p| p.exists())
            .collect()
    } else {
        vec![root_dir.to_path_buf()]
    };

    let mut document_files: Vec<PathBuf> = Vec::new();
    for search_root in &search_roots {
        for entry in WalkDir::new(search_root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if document_files.len() >= MAX_DOCUMENTS_TO_SEARCH {
                break;
            }

            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Check extension
            let ext = match path.extension().and_then(|e| e.to_str()) {
                Some(e) => e.to_ascii_lowercase(),
                None => continue,
            };

            if !DOCUMENT_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }

            // Skip large files
            if let Ok(metadata) = path.metadata()
                && metadata.len() > MAX_SEARCH_DOCUMENT_SIZE
            {
                continue;
            }

            document_files.push(path.to_path_buf());
        }
    }

    if document_files.is_empty() {
        return Vec::new();
    }

    // Initialize converter once
    let converter = match Converter::new() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut results: Vec<DocumentMatchResult> = Vec::new();

    for doc_path in &document_files {
        let path_str = match doc_path.to_str() {
            Some(s) => s,
            None => continue,
        };

        let ext = doc_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let format_name = match ext.as_str() {
            "pdf" => "PDF",
            "docx" => "DOCX",
            "xlsx" => "XLSX",
            "pptx" => "PPTX",
            "odt" => "ODT",
            "rtf" => "RTF",
            _ => continue,
        };

        // Convert with split_pages so we can report page numbers
        let conversion = converter
            .convert(path_str)
            .to(OutputFormat::Markdown {
                split_pages: true,
                optimize_for_llm: true,
            })
            .execute()
            .await;

        let conversion_result = match conversion {
            Ok(r) => r,
            Err(_) => continue,
        };

        // Relative path for display
        let rel_path = doc_path
            .strip_prefix(root_dir)
            .unwrap_or(doc_path)
            .to_string_lossy()
            .to_string();

        // Search each page
        for (page_idx, page_output) in conversion_result.content.iter().enumerate() {
            let page_text = match String::from_utf8(page_output.data.clone()) {
                Ok(t) => t,
                Err(_) => continue,
            };

            let lines: Vec<&str> = page_text.lines().collect();
            if lines.is_empty() {
                continue;
            }

            // Find all matches and map them to line numbers
            let mut line_matches: Vec<(usize, usize, usize)> = Vec::new(); // (line_idx, start_in_line, end_in_line)

            // Build line start offset index
            let mut line_starts: Vec<usize> = Vec::new();
            let mut offset = 0;
            for line in &lines {
                line_starts.push(offset);
                offset += line.len() + 1; // +1 for newline
            }

            for m in regex.find_iter(&page_text) {
                let match_start = m.start();
                let match_end = m.end();

                // Find which line this match starts on
                let line_idx = match line_starts.binary_search(&match_start) {
                    Ok(idx) => idx,
                    Err(idx) => idx.saturating_sub(1),
                };

                if line_idx >= lines.len() {
                    continue;
                }

                let line_offset = line_starts[line_idx];
                let start_in_line = match_start - line_offset;
                let end_in_line = (match_end - line_offset).min(lines[line_idx].len());

                line_matches.push((line_idx, start_in_line, end_in_line));
            }

            if line_matches.is_empty() {
                continue;
            }

            // Group matches into sections (merge when context overlaps)
            let sections = group_matches_into_sections(&line_matches, lines.len());

            for section in sections {
                let section_start = section.start_line;
                let section_end = section.end_line;

                let line_content: Vec<String> = lines[section_start..=section_end]
                    .iter()
                    .map(|l| l.to_string())
                    .collect();

                // Build match_lines and match_ranges relative to the section
                let mut match_lines: Vec<usize> = Vec::new();
                let mut match_ranges: Vec<Vec<(usize, usize)>> = Vec::new();

                for &(line_idx, start, end) in &section.matches {
                    let rel_line = line_idx - section_start;
                    if let Some(pos) = match_lines.iter().position(|&x| x == rel_line) {
                        match_ranges[pos].push((start, end));
                    } else {
                        match_lines.push(rel_line);
                        match_ranges.push(vec![(start, end)]);
                    }
                }

                results.push(DocumentMatchResult {
                    file: rel_path.clone(),
                    format: format_name.to_string(),
                    page: page_idx + 1,
                    line_content,
                    start_line: section_start,
                    match_lines,
                    match_ranges,
                    match_count: section.matches.len(),
                });
            }
        }
    }

    results
}

/// A grouped section of nearby matches.
struct MatchSection {
    start_line: usize,
    end_line: usize,
    matches: Vec<(usize, usize, usize)>, // (line_idx, start_in_line, end_in_line)
}

/// Group matches into sections, merging when their context lines would overlap.
fn group_matches_into_sections(
    line_matches: &[(usize, usize, usize)],
    total_lines: usize,
) -> Vec<MatchSection> {
    if line_matches.is_empty() {
        return Vec::new();
    }

    let mut sections: Vec<MatchSection> = Vec::new();

    for &(line_idx, start, end) in line_matches {
        let context_start = line_idx.saturating_sub(CONTEXT_LINES);
        let context_end = (line_idx + CONTEXT_LINES).min(total_lines - 1);

        // Try to merge with the last section if overlapping
        if let Some(last) = sections.last_mut()
            && context_start <= last.end_line + 1
        {
            // Extend the section
            last.end_line = last.end_line.max(context_end);
            last.matches.push((line_idx, start, end));
            continue;
        }

        // Start a new section
        sections.push(MatchSection {
            start_line: context_start,
            end_line: context_end,
            matches: vec![(line_idx, start, end)],
        });
    }

    sections
}
