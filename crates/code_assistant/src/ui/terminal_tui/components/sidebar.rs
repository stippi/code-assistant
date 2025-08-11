use crate::persistence::ChatMetadata;
use crate::session::instance::SessionActivityState;
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

pub struct SidebarComponent {
    list_state: ListState,
    visible: bool,
}

impl SidebarComponent {
    pub fn new() -> Self {
        Self {
            list_state: ListState::default(),
            visible: false,
        }
    }

    pub fn toggle_visibility(&mut self) {
        self.visible = !self.visible;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        sessions: &[ChatMetadata],
        current_session_id: Option<&str>,
        activity_states: &std::collections::HashMap<String, SessionActivityState>,
    ) {
        if !self.visible {
            return;
        }

        // Create list items
        let items: Vec<ListItem> = sessions
            .iter()
            .map(|session| {
                let mut spans = vec![
                    Span::raw(&session.name),
                ];

                // Add activity indicator
                if let Some(activity_state) = activity_states.get(&session.id) {
                    let indicator = match activity_state {
                        SessionActivityState::Idle => "",
                        SessionActivityState::AgentRunning => " 🔄",
                        SessionActivityState::WaitingForResponse => " ⏳",
                        SessionActivityState::RateLimited { .. } => " ⏰",
                    };
                    if !indicator.is_empty() {
                        spans.push(Span::styled(indicator, Style::default().fg(Color::Yellow)));
                    }
                }

                // Highlight current session
                let style = if current_session_id.map(|id| id == session.id).unwrap_or(false) {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                };

                ListItem::new(Line::from(spans)).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title("Sessions (Ctrl+S: toggle, ↑↓: navigate, Enter: select)")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue)),
            )
            .highlight_style(Style::default().fg(Color::Black).bg(Color::White));

        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    pub fn next(&mut self, sessions_len: usize) {
        if sessions_len == 0 {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= sessions_len - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn previous(&mut self, sessions_len: usize) {
        if sessions_len == 0 {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    sessions_len - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn get_selected_session_id(&self, sessions: &[ChatMetadata]) -> Option<String> {
        if let Some(selected) = self.list_state.selected() {
            sessions.get(selected).map(|s| s.id.clone())
        } else {
            None
        }
    }
}
