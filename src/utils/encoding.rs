use crate::types::FileEncoding;
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
            if let Ok(_) = std::str::from_utf8(&bytes) {
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
