//! Document search support for search_files.
//!
//! When the `document-conversion` feature is enabled, this module walks the project
//! directory looking for supported document files (PDF, DOCX, XLSX, PPTX, ODT, RTF),
//! converts them to Markdown page-by-page using `transmutation`, and searches the
//! resulting text with the user's regex pattern. Matches are reported with page numbers.

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

/// Context characters around a match in the excerpt.
const EXCERPT_CONTEXT: usize = 80;

/// Search for `regex_pattern` within document files under `root_dir`.
///
/// If `paths` is provided, only documents under those directories are searched.
/// Returns a list of matches with page numbers and excerpts.
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

        // Search each page
        for (page_idx, page_output) in conversion_result.content.iter().enumerate() {
            let page_text = match String::from_utf8(page_output.data.clone()) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let matches: Vec<_> = regex.find_iter(&page_text).collect();

            if matches.is_empty() {
                continue;
            }

            // Build excerpt around the first match
            let first_match = &matches[0];
            let start = first_match.start().saturating_sub(EXCERPT_CONTEXT);
            let end = (first_match.end() + EXCERPT_CONTEXT).min(page_text.len());

            // Snap to char boundaries
            let mut excerpt_start = start;
            while excerpt_start < page_text.len() && !page_text.is_char_boundary(excerpt_start) {
                excerpt_start += 1;
            }
            let mut excerpt_end = end;
            while excerpt_end < page_text.len() && !page_text.is_char_boundary(excerpt_end) {
                excerpt_end += 1;
            }

            let mut excerpt = String::new();
            if excerpt_start > 0 {
                excerpt.push_str("...");
            }
            excerpt.push_str(&page_text[excerpt_start..excerpt_end]);
            if excerpt_end < page_text.len() {
                excerpt.push_str("...");
            }

            // Relative path for display
            let rel_path = doc_path
                .strip_prefix(root_dir)
                .unwrap_or(doc_path)
                .to_string_lossy()
                .to_string();

            results.push(DocumentMatchResult {
                file: rel_path,
                format: format_name.to_string(),
                page: page_idx + 1,
                excerpt,
                match_count: matches.len(),
            });
        }
    }

    results
}
