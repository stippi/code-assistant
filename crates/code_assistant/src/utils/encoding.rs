use crate::types::{FileEncoding, FileFormat, LineEnding};
use anyhow::Result;
use content_inspector::{self, ContentType};
use encoding_rs::{Encoding, UTF_8};
use std::path::Path;

/// Detects if a file is a text file by checking both extension and content
pub fn is_text_file(path: &Path) -> bool {
    // Common text file extensions for quick filtering
    let text_extensions = [
        "txt",
        "md",
        "rs",
        "js",
        "py",
        "java",
        "c",
        "cpp",
        "h",
        "hpp",
        "css",
        "html",
        "xml",
        "json",
        "yaml",
        "yml",
        "toml",
        "sh",
        "bash",
        "zsh",
        "fish",
        "conf",
        "cfg",
        "ini",
        "properties",
        "env",
    ];

    // Fast path: first check the extension
    let is_known_text_extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| text_extensions.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false);

    if is_known_text_extension {
        return true;
    }

    // For unknown extensions, we need to check the content more carefully
    match std::fs::read(path) {
        Ok(buffer) => {
            // Only examine the first 1024 bytes for performance
            let sample = if buffer.len() > 1024 {
                &buffer[..1024]
            } else {
                &buffer
            };

            // Use content_inspector to check content type
            // Consider all text formats (UTF-8, UTF-16) as text files
            match content_inspector::inspect(sample) {
                ContentType::BINARY => false,
                _ => true, // UTF8, UTF16LE, UTF16BE are all text
            }
        }
        Err(_) => false, // Couldn't read the file
    }
}

/// Detects the encoding of a file and returns the content as UTF-8 string
pub fn read_file_with_encoding(path: &Path) -> Result<(String, FileEncoding)> {
    let bytes = std::fs::read(path)?;

    // Try to detect encoding with encoding_rs
    let (encoding, confidence) = encoding_rs::Encoding::for_bom(&bytes)
        .or_else(|| {
            // Try direct UTF-8 validation first (most common case)
            if std::str::from_utf8(&bytes).is_ok() {
                return Some((UTF_8, 100));
            }

            // Try common encodings
            for enc in &[
                encoding_rs::UTF_16LE,
                encoding_rs::UTF_16BE,
                encoding_rs::WINDOWS_1252,
                encoding_rs::ISO_8859_2,
            ] {
                let (_result, _, had_errors) = enc.decode(&bytes);
                if !had_errors {
                    return Some((enc, 80));
                }
            }
            None
        })
        .unwrap_or((UTF_8, 50));

    // Convert to actual content
    let (cow, _enc_used, had_errors) = encoding.decode(&bytes);

    // Map encoding to our enum
    let file_encoding = match encoding.name() {
        "UTF-8" => FileEncoding::UTF8,
        "UTF-16LE" => FileEncoding::UTF16LE,
        "UTF-16BE" => FileEncoding::UTF16BE,
        "windows-1252" => FileEncoding::Windows1252,
        "ISO-8859-2" => FileEncoding::ISO8859_2,
        other => FileEncoding::Other(other.to_string()),
    };

    if had_errors {
        tracing::warn!(
            "File {} had encoding errors when decoded as {}, confidence: {}",
            path.display(),
            encoding.name(),
            confidence
        );
    }

    Ok((cow.into_owned(), file_encoding))
}

/// Writes content to a file using the specified encoding
pub fn write_file_with_encoding(path: &Path, content: &str, encoding: &FileEncoding) -> Result<()> {
    let encoding = match encoding {
        FileEncoding::UTF8 => encoding_rs::UTF_8,
        FileEncoding::UTF16LE => encoding_rs::UTF_16LE,
        FileEncoding::UTF16BE => encoding_rs::UTF_16BE,
        FileEncoding::Windows1252 => encoding_rs::WINDOWS_1252,
        FileEncoding::ISO8859_2 => encoding_rs::ISO_8859_2,
        FileEncoding::Other(name) => {
            Encoding::for_label(name.as_bytes()).unwrap_or(encoding_rs::UTF_8)
        }
    };

    let (bytes, _, _) = encoding.encode(content);
    std::fs::write(path, &bytes)?;
    Ok(())
}

/// Detects the line ending used in a string
pub fn detect_line_ending(content: &str) -> LineEnding {
    if content.contains("\r\n") {
        LineEnding::Crlf
    } else if content.contains('\r') && !content.contains('\n') {
        LineEnding::CR
    } else {
        LineEnding::LF
    }
}

/// Normalizes text content by:
/// 1. Converting all line endings to LF (\n)
/// 2. Removing trailing whitespace from each line
/// 3. Preserving empty lines
/// 4. NOT adding a trailing newline (this happens at file write time)
pub fn normalize_content(content: &str) -> String {
    // First normalize all line endings to LF
    let content = content.replace("\r\n", "\n").replace('\r', "\n");

    // Process each line - preserve empty lines but trim trailing whitespace
    content
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Restores the original line endings of content
pub fn restore_format(content: &str, format: &FileFormat) -> String {
    let mut result = content.to_string();

    // Restore line endings
    match format.line_ending {
        LineEnding::Crlf => {
            result = result.replace('\n', "\r\n");
        }
        LineEnding::CR => {
            result = result.replace('\n', "\r");
        }
        LineEnding::LF => {} // Already in LF format
    }

    result
}

/// Writes content to a file using the specified file format
/// Ensures the file ends with a newline
pub fn write_file_with_format(path: &Path, content: &str, format: &FileFormat) -> Result<()> {
    // First restore the original format
    let mut formatted_content = restore_format(content, format);

    // Ensure content ends with exactly one newline
    let line_ending = match format.line_ending {
        LineEnding::Crlf => "\r\n",
        LineEnding::CR => "\r",
        LineEnding::LF => "\n",
    };

    // Remove any trailing newlines
    while formatted_content.ends_with(line_ending) {
        formatted_content =
            formatted_content[..formatted_content.len() - line_ending.len()].to_string();
    }

    // Add exactly one newline
    formatted_content.push_str(line_ending);

    // Then write with the correct encoding
    write_file_with_encoding(path, &formatted_content, &format.encoding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileFormat, LineEnding};

    #[test]
    fn test_normalize_content() {
        let input_cases = [
            // Test case with trailing whitespace
            "Line1  \nLine2 \nLine3",
            // Test case with mixed line endings
            "Line1\r\nLine2\rLine3\n",
            // Test case with both
            "Line1  \r\nLine2 \rLine3 \n",
        ];

        // After normalization, there should be no trailing newline and no trailing whitespace
        let expected_outputs = [
            "Line1\nLine2\nLine3",
            "Line1\nLine2\nLine3",
            "Line1\nLine2\nLine3",
        ];

        for (input, expected) in input_cases.iter().zip(expected_outputs.iter()) {
            let result = normalize_content(input);
            assert_eq!(&result, expected);
        }
    }

    #[test]
    fn test_detect_line_ending() {
        assert_eq!(detect_line_ending("Line1\nLine2\nLine3"), LineEnding::LF);
        assert_eq!(
            detect_line_ending("Line1\r\nLine2\r\nLine3"),
            LineEnding::Crlf
        );
        assert_eq!(detect_line_ending("Line1\rLine2\rLine3"), LineEnding::CR);
        // Mixed should prioritize CRLF
        assert_eq!(
            detect_line_ending("Line1\r\nLine2\nLine3"),
            LineEnding::Crlf
        );
    }

    #[test]
    fn test_restore_format() {
        let lf_content = "Line1\nLine2\nLine3";

        let crlf_format = FileFormat {
            encoding: FileEncoding::UTF8,
            line_ending: LineEnding::Crlf,
        };

        let cr_format = FileFormat {
            encoding: FileEncoding::UTF8,
            line_ending: LineEnding::CR,
        };

        assert_eq!(
            restore_format(lf_content, &crlf_format),
            "Line1\r\nLine2\r\nLine3"
        );
        assert_eq!(
            restore_format(lf_content, &cr_format),
            "Line1\rLine2\rLine3"
        );
    }

    #[test]
    fn test_file_format_with_newlines() {
        // Test cases with different combinations of trailing newlines
        let test_cases = [
            // No trailing newline
            "Line1\nLine2\nLine3",
            // With trailing newline
            "Line1\nLine2\nLine3\n",
            // With multiple trailing newlines
            "Line1\nLine2\nLine3\n\n",
        ];

        let format = FileFormat {
            encoding: FileEncoding::UTF8,
            line_ending: LineEnding::LF,
        };

        // All cases should result in content with exactly one trailing newline
        let expected = "Line1\nLine2\nLine3\n";

        for input in test_cases {
            // We can't directly test write_file_with_format without file I/O,
            // but we can test the transformation logic
            let mut content = restore_format(input, &format);

            // Apply the same logic as in write_file_with_format
            while content.ends_with('\n') {
                content = content[..content.len() - 1].to_string();
            }
            content.push('\n');

            assert_eq!(content, expected);
        }
    }
}
