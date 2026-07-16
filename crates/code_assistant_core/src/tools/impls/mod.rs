// Tool implementations
pub mod browser;
pub mod delete_files;
pub mod edit;
pub mod execute_command;
pub mod glob_files;
pub mod goal;
pub mod list_files;
pub mod list_projects;
pub mod list_skills;
pub mod name_session;
pub mod perplexity_ask;
pub mod read_files;
pub mod read_skill;
pub mod replace_in_file;
pub mod search_files;
pub mod spawn_agent;
pub mod update_plan;
pub mod view_documents;
pub mod view_images;
pub mod wakeup;
pub mod web_fetch;
pub mod web_search;
pub mod write_file;
pub mod write_stdin;

// Re-export all tools for registration
pub use browser::{
    BrowserActTool, BrowserCloseTool, BrowserLoginTool, BrowserNavigateTool, BrowserProfilesTool,
    BrowserReadTool,
};
pub use delete_files::DeleteFilesTool;
pub use edit::EditTool;
pub use execute_command::ExecuteCommandTool;
pub use glob_files::GlobFilesTool;
pub use goal::GoalTool;
pub use list_files::ListFilesTool;
pub use list_projects::ListProjectsTool;
pub use list_skills::ListSkillsTool;
pub use name_session::NameSessionTool;
pub use perplexity_ask::PerplexityAskTool;
pub use read_files::ReadFilesTool;
pub use read_skill::ReadSkillTool;
pub use replace_in_file::ReplaceInFileTool;
pub use search_files::SearchFilesTool;
pub use spawn_agent::SpawnAgentTool;
pub use update_plan::UpdatePlanTool;
pub use view_documents::ViewDocumentsTool;
pub use view_images::ViewImagesTool;
pub use wakeup::{CancelWakeupTool, ScheduleWakeupTool};
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
pub use write_file::WriteFileTool;
pub use write_stdin::WriteStdinTool;
