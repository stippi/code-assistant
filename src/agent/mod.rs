#[cfg(test)]
mod tests;

mod agent;
mod agent_chat;

pub use agent::Agent;
pub use agent_chat::AgentChat;

pub enum ToolMode {
    Native,
    Xml,
}
