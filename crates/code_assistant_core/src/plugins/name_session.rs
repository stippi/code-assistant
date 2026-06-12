//! Session naming: the hidden `name_session` tool and the reminder that
//! nudges the LLM to call it.

use crate::agent::hooks::{IterationHook, LoopCtx, ToolInterceptor};
use crate::agent::types::ToolExecution;
use crate::plugins::AgentAppState;
use crate::tools::ToolRequest;
use anyhow::Result;
use llm::{ContentBlock, Message, MessageContent, MessageRole};
use tracing::{trace, warn};

/// Handles the `name_session` tool at the agent level: the title is session
/// state, not a real tool execution.
pub struct NameSessionInterceptor;

impl ToolInterceptor for NameSessionInterceptor {
    fn try_intercept(&self, request: &ToolRequest, ctx: &mut LoopCtx) -> Option<Result<bool>> {
        if request.name != "name_session" {
            return None;
        }
        Some(apply_session_name(request, ctx))
    }
}

fn apply_session_name(request: &ToolRequest, ctx: &mut LoopCtx) -> Result<bool> {
    if let Some(title) = request.input["title"].as_str() {
        let title = title.trim();
        if !title.is_empty() {
            trace!("Obtained session title from LLM: {}", title);
            AgentAppState::of(ctx.extensions).session_name = title.to_string();

            ctx.tool_executions.push(ToolExecution {
                tool_request: request.clone(),
                result: Box::new(crate::tools::impls::name_session::NameSessionOutput {
                    title: title.to_string(),
                }),
            });
            return Ok(true);
        } else {
            warn!("Title for name_session is empty after trimming");
        }
    } else {
        warn!("No 'title' field found in name_session input or it's not a string");
    }
    Err(anyhow::anyhow!("Invalid session title provided"))
}

/// Appends a system reminder to the last actual user message while the
/// session has no name yet.
pub struct NameSessionReminderHook;

impl IterationHook for NameSessionReminderHook {
    fn shape_request(&self, messages: &mut Vec<Message>, ctx: &LoopCtx) -> Result<()> {
        let state = AgentAppState::of_ref(&*ctx.extensions);

        // Only inject if enabled, session is not named yet, and we have messages
        if !state.naming_reminders_enabled || !state.session_name.is_empty() || messages.is_empty()
        {
            return Ok(());
        }

        // Skip the reminder when the `name_session` tool isn't available in the
        // current tool scope (e.g. for sub-agents). Otherwise we'd nag the agent
        // to call a tool it cannot use.
        if !ctx
            .registry
            .tool_has_capability("name_session", state.tool_scope.tag())
        {
            return Ok(());
        }

        // Find the last actual user message (not tool results) and add system reminder
        // Iterate backwards through messages to find the last user message with actual content
        for msg in messages.iter_mut().rev() {
            if matches!(msg.role, MessageRole::User) {
                let is_actual_user_message = match &msg.content {
                    MessageContent::Text(_) => true, // Text content is always actual user input
                    MessageContent::Structured(blocks) => {
                        // Check if this message contains tool results
                        // If it contains only ToolResult blocks, it's not an actual user message
                        blocks
                            .iter()
                            .any(|block| !matches!(block, ContentBlock::ToolResult { .. }))
                    }
                };

                if is_actual_user_message {
                    let reminder_text = "<system-reminder>\nThis is an automatic reminder from the system. Please use the `name_session` tool first, provided the user has already given you a clear task or question. You can chain additional tools after using the `name_session` tool.\n</system-reminder>";

                    trace!("Injecting session naming reminder to actual user message");
                    msg.volatile = true;

                    match &mut msg.content {
                        MessageContent::Text(original_text) => {
                            // Convert from Text to Structured with two ContentBlocks
                            msg.content = MessageContent::Structured(vec![
                                ContentBlock::new_text(original_text.clone()),
                                ContentBlock::new_text(reminder_text.to_string()),
                            ]);
                        }
                        MessageContent::Structured(blocks) => {
                            // Add reminder as a new ContentBlock
                            blocks.push(ContentBlock::new_text(reminder_text));
                        }
                    }
                    break; // Found and updated the last actual user message, we're done
                }
            }
        }

        Ok(())
    }
}
