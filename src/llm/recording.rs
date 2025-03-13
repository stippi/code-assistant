use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{Seek, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Recording session that contains the original request and all chunks
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecordingSession {
    /// The request that was sent (simplified for storage)
    pub request: serde_json::Value,
    /// Timestamp of when the recording was started
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Raw chunks as received from the API
    pub chunks: Vec<RecordedChunk>,
}

/// Single recorded chunk with timing info
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecordedChunk {
    /// Raw content of the data part of the SSE
    pub data: String,
    /// Milliseconds since recording start
    pub timestamp_ms: u64,
}

/// Recorder for API responses
pub struct APIRecorder {
    file_path: Arc<Mutex<Option<String>>>,
    current_session: Arc<Mutex<Option<RecordingSession>>>,
    start_time: Arc<Mutex<Option<Instant>>>,
}

impl APIRecorder {
    /// Create a new recorder that writes to the specified file
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            file_path: Arc::new(Mutex::new(Some(
                path.as_ref().to_string_lossy().to_string(),
            ))),
            current_session: Arc::new(Mutex::new(None)),
            start_time: Arc::new(Mutex::new(None)),
        }
    }

    /// Start a new recording session
    pub fn start_recording(&self, request: serde_json::Value) -> Result<()> {
        let mut session_guard = self.current_session.lock().unwrap();
        let mut start_guard = self.start_time.lock().unwrap();

        // Create new session
        *session_guard = Some(RecordingSession {
            request,
            timestamp: chrono::Utc::now(),
            chunks: Vec::new(),
        });

        // Record start time
        *start_guard = Some(Instant::now());

        Ok(())
    }

    /// Record an incoming chunk
    pub fn record_chunk(&self, data: &str) -> Result<()> {
        let mut session_guard = self.current_session.lock().unwrap();
        let start_guard = self.start_time.lock().unwrap();

        if let (Some(session), Some(start_time)) = (session_guard.as_mut(), *start_guard) {
            let elapsed = start_time.elapsed();
            let timestamp_ms = elapsed.as_secs() * 1000 + elapsed.subsec_millis() as u64;

            session.chunks.push(RecordedChunk {
                data: data.to_string(),
                timestamp_ms,
            });
        }

        Ok(())
    }

    /// End the current recording session and save to disk
    pub fn end_recording(&self) -> Result<()> {
        let file_path_guard = self.file_path.lock().unwrap();
        let mut session_guard = self.current_session.lock().unwrap();
        let mut start_guard = self.start_time.lock().unwrap();

        if let (Some(file_path), Some(session)) = (file_path_guard.as_ref(), session_guard.take()) {
            // Create/open the file
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .append(true)
                .open(file_path)
                .context("Failed to open recording file")?;

            // Serialize and write the session
            let json = serde_json::to_string_pretty(&session)?;
            if let Ok(metadata) = std::fs::metadata(file_path) {
                let file_size = metadata.len();

                if file_size == 0 {
                    // If file is empty, start a JSON array
                    writeln!(file, "[")?;
                } else {
                    // If file already has content, add a comma
                    // Go to position before the last bracket
                    file.set_len(file_size - 1)?;
                    file.seek(std::io::SeekFrom::End(0))?;
                    writeln!(file, ",")?;
                }
            }

            // Write the session
            let mut file = OpenOptions::new()
                .write(true)
                .append(true)
                .open(file_path)
                .context("Failed to open recording file")?;

            writeln!(file, "{}", json)?;
            writeln!(file, "]")?;
        }

        // Reset start time
        *start_guard = None;

        Ok(())
    }
}
