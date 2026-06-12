use gpui::AppContext as _;
use gpui::{App, Entity};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use terminal::Terminal;
use tracing::warn;

/// Global terminal pool singleton.
static TERMINAL_POOL: OnceLock<Mutex<TerminalPool>> = OnceLock::new();

/// Metadata associated with a terminal in the pool.
pub struct TerminalEntry {
    pub terminal: Entity<Terminal>,
    #[allow(dead_code)]
    pub command: String,
}

/// A pool of live PTY terminal entities, keyed by terminal_id.
///
/// Also maintains a secondary index from `(session_id, tool_id)` to `terminal_id`
/// so the UI can look up the right terminal for each tool block.
pub struct TerminalPool {
    /// Primary store: terminal_id → entry
    terminals: HashMap<String, TerminalEntry>,
    /// Secondary index: (session_id, tool_id) → terminal_id
    tool_index: HashMap<(String, String), String>,
    /// Counter for generating unique terminal IDs
    next_id: usize,
}

impl TerminalPool {
    fn new() -> Self {
        Self {
            terminals: HashMap::new(),
            tool_index: HashMap::new(),
            next_id: 0,
        }
    }

    /// Access the global pool (creates it on first call).
    pub fn global() -> &'static Mutex<TerminalPool> {
        TERMINAL_POOL.get_or_init(|| Mutex::new(TerminalPool::new()))
    }

    /// Generate a unique terminal ID and insert a terminal into the pool.
    /// Returns the generated terminal_id.
    pub fn insert(&mut self, terminal: Entity<Terminal>, command: String) -> String {
        let id = format!("gpui-term-{}", self.next_id);
        self.next_id += 1;
        self.terminals
            .insert(id.clone(), TerminalEntry { terminal, command });
        id
    }

    /// Register the mapping from (session_id, tool_id) to terminal_id.
    pub fn register_tool_mapping(
        &mut self,
        session_id: String,
        tool_id: String,
        terminal_id: String,
    ) {
        if let Some(previous_terminal_id) = self
            .tool_index
            .insert((session_id.clone(), tool_id.clone()), terminal_id.clone())
        {
            if previous_terminal_id != terminal_id {
                warn!(
                    "TerminalPool remapped terminal: session='{}', tool_id='{}', old_terminal='{}', new_terminal='{}'",
                    session_id, tool_id, previous_terminal_id, terminal_id
                );
            }
        }
    }

    /// Look up a terminal by its terminal_id.
    #[allow(dead_code)]
    pub fn get(&self, terminal_id: &str) -> Option<&TerminalEntry> {
        self.terminals.get(terminal_id)
    }

    /// Look up a terminal entity by (session_id, tool_id).
    #[allow(dead_code)]
    pub fn get_by_tool(&self, session_id: &str, tool_id: &str) -> Option<&TerminalEntry> {
        self.tool_index
            .get(&(session_id.to_string(), tool_id.to_string()))
            .and_then(|terminal_id| self.terminals.get(terminal_id))
    }

    /// Look up just the terminal entity by tool_id (searches all sessions).
    /// This is a convenience for the output renderer which may not know the session_id.
    pub fn get_terminal_by_tool_id_any_session(&self, tool_id: &str) -> Option<&TerminalEntry> {
        for ((_, tid), terminal_id) in &self.tool_index {
            if tid == tool_id {
                return self.terminals.get(terminal_id);
            }
        }
        None
    }

    /// Remove a terminal from the pool.
    /// Returns the tool IDs whose mappings pointed at this terminal so callers
    /// can clean up any renderer-side caches as well.
    pub fn remove(&mut self, terminal_id: &str) -> Vec<String> {
        self.terminals.remove(terminal_id);
        let removed_tool_ids: Vec<String> = self
            .tool_index
            .iter()
            .filter(|(_, tid)| *tid == terminal_id)
            .map(|((_, tool_id), _)| tool_id.clone())
            .collect();
        self.tool_index.retain(|_, tid| tid != terminal_id);
        removed_tool_ids
    }

    /// Remove all terminals for a given session.
    #[allow(dead_code)]
    pub fn remove_session(&mut self, session_id: &str) {
        let terminal_ids: Vec<String> = self
            .tool_index
            .iter()
            .filter(|((sid, _), _)| sid == session_id)
            .map(|(_, tid)| tid.clone())
            .collect();

        for tid in &terminal_ids {
            self.terminals.remove(tid);
        }
        self.tool_index.retain(|(sid, _), _| sid != session_id);
    }
}

/// Convenience: spawn a PTY terminal and add it to the global pool.
/// Must be called on the GPUI foreground thread.
pub fn spawn_terminal_in_pool(
    command: &str,
    working_dir: Option<&std::path::Path>,
    cx: &mut App,
) -> Result<(String, Entity<Terminal>), anyhow::Error> {
    let options = terminal::TerminalOptions {
        command: Some(command.to_string()),
        working_dir: working_dir.map(|p| p.to_path_buf()),
        env: vec![("TERM".into(), "xterm-256color".into())],
        scroll_history: Some(10_000),
    };

    let builder = terminal::TerminalBuilder::new(options)?;
    let terminal = cx.new(|cx| builder.subscribe(cx));

    let terminal_id = {
        let mut pool = TerminalPool::global()
            .lock()
            .map_err(|e| anyhow::anyhow!("Terminal pool lock poisoned: {e}"))?;
        pool.insert(terminal.clone(), command.to_string())
    };

    Ok((terminal_id, terminal))
}
