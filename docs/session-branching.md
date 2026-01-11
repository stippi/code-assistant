# Session Branching Feature

## Overview

Session Branching ermöglicht das Erstellen von alternativen Gesprächsverläufen durch Editieren bereits abgeschickter User-Nachrichten. Statt den ursprünglichen Verlauf zu löschen, entstehen "Abzweigungen" (Branches), zwischen denen der User umschalten kann.

### Ziele

1. **Exploration**: "Was wäre wenn ich etwas anders formuliert hätte?"
2. **Preservation**: Bestehende Verläufe bleiben erhalten
3. **Eleganz**: Einfache, mächtige Datenstruktur

## Architektur-Design

### Kernkonzept: Baum statt Liste

Die aktuelle Architektur speichert Messages als lineare Liste (`Vec<Message>`). Das neue Design verwendet einen **gerichteten Baum** (DAG - Directed Acyclic Graph):

```
                    ┌─────────────────┐
                    │  Root (Start)   │
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │  User: "Hi"     │  node_id: 1
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │  Assistant...   │  node_id: 2
                    └────────┬────────┘
                             │
          ┌──────────────────┼──────────────────┐
          │                  │                  │
 ┌────────▼────────┐ ┌───────▼────────┐ ┌───────▼────────┐
 │ User: "Fix A"   │ │ User: "Fix B"  │ │ User: "Fix C"  │
 │ (original)      │ │ (branch 1)     │ │ (branch 2)     │
 │ node_id: 3      │ │ node_id: 5     │ │ node_id: 7     │
 └────────┬────────┘ └────────┬───────┘ └────────┬───────┘
          │                   │                  │
 ┌────────▼────────┐ ┌────────▼───────┐ ┌────────▼───────┐
 │  Assistant...   │ │  Assistant...  │ │  Assistant...  │
 │  node_id: 4     │ │  node_id: 6    │ │  node_id: 8    │
 └─────────────────┘ └────────────────┘ └────────────────┘
```

### Datenstrukturen

#### MessageNode

Ersetzt `Message` als Speichereinheit innerhalb einer Session:

```rust
// In crates/code_assistant/src/persistence.rs

/// Unique identifier for a message node within a session
pub type NodeId = u64;

/// A single message node in the conversation tree
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageNode {
    /// Unique ID within this session
    pub id: NodeId,

    /// The actual message content
    pub message: Message,

    /// Parent node ID (None for root/first message)
    pub parent_id: Option<NodeId>,

    /// Creation timestamp (for ordering siblings)
    pub created_at: SystemTime,

    /// Plan state snapshot (only set if plan changed in this message's response)
    /// Used for efficient plan reconstruction when switching branches
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_snapshot: Option<PlanState>,
}

/// A path through the conversation tree (list of node IDs from root to leaf)
pub type ConversationPath = Vec<NodeId>;
```

#### Erweiterte ChatSession

```rust
// In crates/code_assistant/src/persistence.rs

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatSession {
    // ... existing fields ...

    /// All message nodes in the session (tree structure)
    /// Key: NodeId, Value: MessageNode
    /// Using BTreeMap for ordered iteration and efficient lookup
    #[serde(default)]
    pub message_nodes: BTreeMap<NodeId, MessageNode>,

    /// The currently active path through the tree
    /// This determines which messages are shown and sent to LLM
    #[serde(default)]
    pub active_path: ConversationPath,

    /// Counter for generating unique node IDs
    #[serde(default)]
    pub next_node_id: NodeId,

    /// Legacy: Old linear message list (for migration)
    /// Will be migrated to message_nodes on first load
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
}
```

#### Branch-Metadaten für UI

```rust
// In crates/code_assistant/src/ui/ui_events.rs

/// Information about a branch point in the conversation
#[derive(Debug, Clone)]
pub struct BranchInfo {
    /// Node ID where the branch occurs (parent node)
    pub branch_point_id: NodeId,

    /// All sibling node IDs (different continuations)
    pub sibling_ids: Vec<NodeId>,

    /// Index of the currently active sibling (0-based)
    pub active_index: usize,

    /// Total number of branches at this point
    pub total_branches: usize,
}
```

### Migration bestehender Sessions

```rust
impl ChatSession {
    /// Migrate legacy linear messages to tree structure
    pub fn migrate_to_tree_structure(&mut self) -> Result<()> {
        if self.message_nodes.is_empty() && !self.messages.is_empty() {
            // Convert linear messages to tree
            let mut parent_id: Option<NodeId> = None;

            for message in self.messages.drain(..) {
                let node_id = self.next_node_id;
                self.next_node_id += 1;

                let node = MessageNode {
                    id: node_id,
                    message,
                    parent_id,
                    created_at: SystemTime::now(),
                };

                self.message_nodes.insert(node_id, node);
                self.active_path.push(node_id);
                parent_id = Some(node_id);
            }
        }
        Ok(())
    }
}
```

## UI-Änderungen

### 1. Edit-Button auf User-Nachrichten

In `crates/code_assistant/src/ui/gpui/messages.rs`:

```rust
// Für User-Nachrichten einen Edit-IconButton hinzufügen
if msg.read(cx).is_user_message() {
    message_container = message_container.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between() // Verteilt Inhalt zwischen links und rechts
            .gap_2()
            .children(vec![
                // Linke Seite: User Badge (wie bisher)
                div().flex().flex_row().items_center().gap_2()
                    .child(/* user icon */)
                    .child(/* "You" label */),

                // Rechte Seite: Edit Button (nur bei Hover sichtbar)
                IconButton::new("edit")
                    .icon(IconName::Pencil)
                    .tooltip("Edit this message and create a new branch")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_message_edit(node_id, window, cx);
                    }))
            ])
    )
}
```

### 2. Branch-Switcher unter User-Nachrichten

Wenn ein Punkt mehrere Branches hat, wird ein kleiner Umschalter angezeigt:

```rust
/// Branch switcher component
pub struct BranchSwitcher {
    branch_info: BranchInfo,
    on_switch: Callback<usize>, // Called with new index
}

impl Render for BranchSwitcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .children([
                // Left arrow button (disabled if at index 0)
                IconButton::new("prev")
                    .icon(IconName::ChevronLeft)
                    .disabled(self.branch_info.active_index == 0)
                    .on_click(/* switch to prev */),

                // "2/3" indicator
                div().child(format!(
                    "{}/{}",
                    self.branch_info.active_index + 1,
                    self.branch_info.total_branches
                )),

                // Right arrow button
                IconButton::new("next")
                    .icon(IconName::ChevronRight)
                    .disabled(self.branch_info.active_index >= self.branch_info.total_branches - 1)
                    .on_click(/* switch to next */),
            ])
    }
}
```

**Darstellung:**
```
┌────────────────────────────────────┐
│ 👤 You                          ✏️ │
│                                    │
│ "Please fix the bug in file.rs"    │
│                                    │
│         ◀  2/3  ▶                  │
└────────────────────────────────────┘
```

### 3. Edit-Workflow

1. User klickt Edit-Button auf einer User-Nachricht
2. Die Message-Inhalte und Attachments werden in den Input-Bereich geladen
3. UI markiert den "Branch-Punkt" (die Node, nach der die neue Nachricht eingefügt wird)
4. User editiert und sendet
5. System erstellt neue MessageNode mit gleichem `parent_id` wie das Original
6. `active_path` wird aktualisiert auf den neuen Branch

```rust
// In input_area.rs oder messages.rs
pub fn start_message_edit(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
    // 1. Find the node
    let node = self.get_node(node_id);

    // 2. Load content into input area
    let content = node.message.extract_text_content();
    let attachments = node.message.extract_attachments();

    // 3. Set editing state (parent of the node being edited)
    self.editing_branch_point = node.parent_id;

    // 4. Populate input
    self.input_area.set_content(content);
    self.input_area.set_attachments(attachments);

    // 5. Focus input
    cx.focus(&self.input_area);
}
```

## Neue UI-Events

```rust
// In crates/code_assistant/src/ui/ui_events.rs

pub enum UiEvent {
    // ... existing events ...

    /// Start editing a message (load content to input, set branch point)
    StartMessageEdit {
        node_id: NodeId,
        session_id: String,
    },

    /// Switch to a different branch at a branch point
    SwitchBranch {
        branch_point_id: NodeId,
        new_active_sibling_id: NodeId,
        session_id: String,
    },

    /// Notify UI about branch info for a node (for rendering branch switcher)
    UpdateBranchInfo {
        node_id: NodeId,
        branch_info: BranchInfo,
    },

    /// Set messages with branch information
    SetMessagesWithBranches {
        messages: Vec<MessageData>,
        branch_infos: Vec<BranchInfo>,
        session_id: Option<String>,
        tool_results: Vec<ToolResultData>,
    },
}
```

## Session-Management Änderungen

### ChatSession Methoden

```rust
impl ChatSession {
    /// Get the linearized message history for the active path
    pub fn get_active_messages(&self) -> Vec<&Message> {
        self.active_path
            .iter()
            .filter_map(|id| self.message_nodes.get(id))
            .map(|node| &node.message)
            .collect()
    }

    /// Get all children of a node (for branch detection)
    pub fn get_children(&self, parent_id: Option<NodeId>) -> Vec<&MessageNode> {
        self.message_nodes
            .values()
            .filter(|node| node.parent_id == parent_id)
            .collect()
    }

    /// Check if a node has multiple children (is a branch point)
    pub fn is_branch_point(&self, node_id: NodeId) -> bool {
        self.get_children(Some(node_id)).len() > 1
    }

    /// Get branch info for a specific node
    pub fn get_branch_info(&self, node_id: NodeId) -> Option<BranchInfo> {
        let node = self.message_nodes.get(&node_id)?;
        let siblings: Vec<_> = self.get_children(node.parent_id)
            .into_iter()
            .map(|n| n.id)
            .collect();

        if siblings.len() <= 1 {
            return None; // No branching here
        }

        let active_index = siblings.iter().position(|&id| id == node_id)?;

        Some(BranchInfo {
            branch_point_id: node.parent_id.unwrap_or(0),
            sibling_ids: siblings,
            active_index,
            total_branches: siblings.len(),
        })
    }

    /// Add a new message as a child of the given parent
    /// Returns the new node ID
    pub fn add_message(&mut self, message: Message, parent_id: Option<NodeId>) -> NodeId {
        let node_id = self.next_node_id;
        self.next_node_id += 1;

        let node = MessageNode {
            id: node_id,
            message,
            parent_id,
            created_at: SystemTime::now(),
        };

        self.message_nodes.insert(node_id, node);
        node_id
    }

    /// Switch to a different branch at a branch point
    /// Updates active_path to follow the new branch
    pub fn switch_branch(&mut self, new_node_id: NodeId) -> Result<()> {
        let node = self.message_nodes.get(&new_node_id)
            .ok_or_else(|| anyhow::anyhow!("Node not found: {}", new_node_id))?;

        // Find where in active_path the parent is
        if let Some(parent_id) = node.parent_id {
            if let Some(parent_pos) = self.active_path.iter().position(|&id| id == parent_id) {
                // Truncate path after parent
                self.active_path.truncate(parent_pos + 1);

                // Build path from new node to deepest descendant on current active path
                self.extend_active_path_from(new_node_id);
            }
        } else {
            // Switching root node
            self.active_path.clear();
            self.extend_active_path_from(new_node_id);
        }

        Ok(())
    }

    /// Extend active_path from a given node, following the "most recent" child
    fn extend_active_path_from(&mut self, start_node_id: NodeId) {
        self.active_path.push(start_node_id);

        let mut current_id = start_node_id;
        loop {
            // Find children of current node
            let mut children: Vec<_> = self.get_children(Some(current_id))
                .into_iter()
                .collect();

            if children.is_empty() {
                break;
            }

            // Sort by created_at, take most recent (or first in existing path)
            children.sort_by_key(|n| n.created_at);

            // Prefer child that was in the original active_path, otherwise most recent
            let next_child = children.last().unwrap();
            self.active_path.push(next_child.id);
            current_id = next_child.id;
        }
    }
}
```

### SessionState Änderungen

```rust
// In crates/code_assistant/src/session/mod.rs

pub struct SessionState {
    pub session_id: String,
    pub name: String,

    // Replace Vec<Message> with tree data
    pub message_nodes: BTreeMap<NodeId, MessageNode>,
    pub active_path: ConversationPath,
    pub next_node_id: NodeId,

    pub tool_executions: Vec<ToolExecution>,
    pub plan: PlanState,
    pub config: SessionConfig,
    pub next_request_id: Option<u64>,
    pub model_config: Option<SessionModelConfig>,
}

impl SessionState {
    /// Get linearized messages for the active path (for LLM requests)
    pub fn get_active_messages(&self) -> Vec<Message> {
        self.active_path
            .iter()
            .filter_map(|id| self.message_nodes.get(id))
            .map(|node| node.message.clone())
            .collect()
    }
}
```

## Agent-Änderungen

### Message-Handling im Agent

```rust
// In crates/code_assistant/src/agent/runner.rs

impl Agent {
    // Die message_history bleibt als "flache" Ansicht für die aktuelle Iteration
    // Sie wird aus dem aktiven Pfad der Session rekonstruiert

    /// Load session state including branch information
    pub async fn load_from_session_state(&mut self, state: SessionState) -> Result<()> {
        // ... existing code ...

        // Load the linearized active path as message history
        self.message_history = state.get_active_messages();

        // Store tree structure for state saving
        self.message_nodes = state.message_nodes;
        self.active_path = state.active_path;
        self.next_node_id = state.next_node_id;

        // ... rest of loading ...
    }

    /// Save state with tree structure
    fn save_state(&mut self) -> Result<()> {
        let session_state = SessionState {
            session_id: self.session_id.clone().unwrap_or_default(),
            name: self.session_name.clone(),
            message_nodes: self.message_nodes.clone(),
            active_path: self.active_path.clone(),
            next_node_id: self.next_node_id,
            // ... other fields ...
        };

        self.state_persistence.save_agent_state(session_state)?;
        Ok(())
    }

    /// Append a message to the active path
    pub fn append_message(&mut self, message: Message) -> Result<()> {
        // Get parent (last node in active path)
        let parent_id = self.active_path.last().copied();

        // Create new node
        let node_id = self.next_node_id;
        self.next_node_id += 1;

        let node = MessageNode {
            id: node_id,
            message: message.clone(),
            parent_id,
            created_at: SystemTime::now(),
        };

        // Add to tree
        self.message_nodes.insert(node_id, node);

        // Extend active path
        self.active_path.push(node_id);

        // Keep linearized history in sync
        self.message_history.push(message);

        self.save_state()?;
        Ok(())
    }
}
```

## Implementierungsplan

### Phase 1: Datenstruktur (Backend)

1. **Neue Typen definieren** (`persistence.rs`)
   - `NodeId` type alias
   - `MessageNode` struct
   - `ConversationPath` type alias
   - `BranchInfo` struct

2. **ChatSession erweitern** (`persistence.rs`)
   - Neue Felder: `message_nodes`, `active_path`, `next_node_id`
   - Migration-Logik für bestehende Sessions
   - Methoden: `get_active_messages()`, `add_message()`, `switch_branch()`, etc.

3. **SessionState anpassen** (`session/mod.rs`)
   - Gleiche Struktur wie ChatSession
   - Hilfsmethoden für Linearisierung

4. **Tests schreiben**
   - Migration von linear zu tree
   - Branch-Erstellung
   - Branch-Switching
   - Pfad-Berechnung

### Phase 2: Agent-Anpassungen

1. **Agent-State erweitern** (`agent/runner.rs`)
   - Neue Felder für Tree-Struktur
   - `append_message()` anpassen

2. **State-Laden/Speichern** (`agent/persistence.rs`)
   - Tree-Struktur in SessionState

3. **LLM-Request-Building**
   - Nur aktiven Pfad an LLM senden

### Phase 3: UI-Backend-Kommunikation

1. **Neue UiEvents** (`ui/ui_events.rs`)
   - `StartMessageEdit`
   - `SwitchBranch`
   - `UpdateBranchInfo`

2. **SessionInstance-Methoden** (`session/instance.rs`)
   - `get_branch_info_for_path()`
   - `generate_session_connect_events()` erweitern

3. **SessionManager-Methoden** (`session/manager.rs`)
   - `switch_branch()`
   - `start_message_edit()`

### Phase 4: UI (GPUI)

1. **Edit-Button** (`gpui/messages.rs`)
   - Auf User-Nachrichten, bei Hover sichtbar
   - Click-Handler

2. **BranchSwitcher-Komponente** (`gpui/branch_switcher.rs` - neu)
   - Compact-Darstellung: "◀ 2/3 ▶"
   - Click-Handler für Navigation

3. **MessagesView erweitern** (`gpui/messages.rs`)
   - Branch-Info pro Message
   - BranchSwitcher rendern wo nötig

4. **InputArea-Anpassungen** (`gpui/input_area.rs`)
   - "Edit mode" state
   - Branch-Point tracking

5. **Root/App-Integration** (`gpui/root.rs`, `gpui/mod.rs`)
   - Event-Handling für Branch-Events
   - State-Management

### Phase 5: Testing & Polish

1. **Integrationstests**
   - Branch-Erstellung durch Edit
   - Branch-Wechsel
   - Agent-Loop mit Branches

2. **UI-Polish**
   - Animationen für Branch-Wechsel
   - Visual Feedback beim Editieren
   - Tooltips

3. **Edge Cases**
   - Leere Sessions
   - Single-Message Sessions
   - Tiefe Verschachtelung

## Datei-Änderungen Übersicht

| Datei | Art | Beschreibung |
|-------|-----|--------------|
| `persistence.rs` | Modify | NodeId, MessageNode, ChatSession-Erweiterung |
| `session/mod.rs` | Modify | SessionState-Anpassung |
| `session/instance.rs` | Modify | Branch-Info-Generierung |
| `session/manager.rs` | Modify | Branch-Switch-Methoden |
| `agent/runner.rs` | Modify | Tree-basierte Message-History |
| `ui/ui_events.rs` | Modify | Neue Branch-Events |
| `ui/gpui/messages.rs` | Modify | Edit-Button, BranchSwitcher-Integration |
| `ui/gpui/branch_switcher.rs` | New | BranchSwitcher-Komponente |
| `ui/gpui/mod.rs` | Modify | Event-Handling |
| `ui/gpui/input_area.rs` | Modify | Edit-Mode |

## Design-Entscheidungen

1. **Tool-Executions bei Branches**: Tool-Executions bleiben wie bisher global in der Session gespeichert (`Vec<SerializedToolExecution>`). Sie werden per `tool_id` auf den konkreten Tool-Aufruf in einer Message gemappt. Da die `tool_id` eindeutig ist, funktioniert das auch mit Branches.

2. **Plan-State bei Branches**: Der Plan ist pfad-spezifisch und wird bei Branch-Wechseln aus dem neuen aktiven Pfad rekonstruiert. Dazu geht man die Message-History rückwärts durch bis zur ersten `MessageNode` mit gesetztem `plan_snapshot` Feld. Das `plan_snapshot` wird beim Speichern einer Assistant-Message gesetzt, wenn diese einen `update_plan` Tool-Aufruf enthielt. Der `plan`-Field in `ChatSession` speichert immer den Plan des aktuell aktiven Pfads.

3. **Compaction bei Branches**: Compaction-Messages sind normale Messages im Baum (nur mit `is_compaction_summary: true` Flag). Wenn Branch A eine Compaction hatte und Branch B nicht, wird bei Wechsel zu B die volle History an das LLM geschickt. Im Agent gilt weiterhin: Messages ab der letzten Compaction-Message im aktiven Pfad werden ans API gesendet.

4. **Maximale Tiefe/Breite**: Keine Limits.

## Beispiel-Szenarien

### Szenario 1: Einfaches Branching

```
1. User: "Fix the bug"
2. Assistant: [fixes bug incorrectly]
3. User klickt Edit auf Nachricht 1
4. User: "Fix the bug in utils.rs, line 42"
5. Assistant: [fixes correct bug]
```

Baum:
```
        [1: "Fix the bug"]
              │
        [2: Assistant]
              │
     ┌────────┴────────┐
[3: "Fix bug"]    [4: "Fix bug in utils.rs"]
     │                  │
[*: Assistant]    [5: Assistant]
```

### Szenario 2: Verschachteltes Branching

User probiert mehrere Ansätze in verschiedenen Zweigen:

```
                    [User: "Build feature X"]
                           │
                    [Assistant: Plan A]
                           │
            ┌──────────────┴──────────────┐
     [User: "Use approach A"]      [User: "Use approach B"]
            │                              │
     [Assistant...]                 [Assistant...]
            │                              │
     ┌──────┴──────┐                      ...
[User: A1]    [User: A2]
```

Der User kann jederzeit zwischen allen Pfaden wechseln.
