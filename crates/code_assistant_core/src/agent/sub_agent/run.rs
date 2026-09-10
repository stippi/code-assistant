use super::*;
use futures::FutureExt;

#[async_trait::async_trait]
impl SubAgentRunner for DefaultSubAgentRunner {
    async fn run(
        &self,
        parent_tool_id: &str,
        instructions: String,
        mode: SubAgentMode,
        require_file_references: bool,
    ) -> Result<SubAgentResult> {
        let registration = self.cancellation_registry.register_run(parent_tool_id);
        let child = registration.cancellation.clone();
        let sub_ui = self.build_sub_agent_ui(
            self.ui.clone(),
            parent_tool_id.to_string(),
            child.flag.clone(),
        );
        let work = async {
            self.parent_cancellation.check()?;
            child.token.check()?;
            let mut agent = tokio::select! {
                biased;
                _ = self.parent_cancellation.cancelled() => return Err(tools_core::Cancelled.into()),
                _ = child.token.cancelled() => return Err(tools_core::Cancelled.into()),
                agent = self.build_agent(parent_tool_id, sub_ui.clone(), self.permission_handler.clone()) => agent?,
            };
            agent.set_cancellation(child.token.clone());
            let scope = match mode {
                SubAgentMode::ReadOnly => ToolScope::SubAgentReadOnly,
                SubAgentMode::Default if self.session_config.use_diff_blocks => {
                    ToolScope::SubAgentDefaultWithDiffBlocks
                }
                SubAgentMode::Default => ToolScope::SubAgentDefault,
            };
            agent.set_tool_scope(scope);
            agent.append_message(Message::new_user(instructions))?;
            let mut answer = String::new();
            for attempt in 0..=2 {
                child.token.check()?;
                let iteration = agent.run_single_iteration().await;
                // Earlier requests and tools can have completed before a later
                // request fails. Preserve their usage as well as their tool list.
                sub_ui.set_usage(compute_sub_agent_usage(
                    &agent.message_history(),
                    &self.model_name,
                ));
                iteration?;
                child.token.check()?;
                answer = extract_last_assistant_text(&agent.message_history()).unwrap_or_default();
                if !require_file_references || has_file_references_with_line_ranges(&answer) {
                    break;
                }
                if attempt == 2 {
                    answer.push_str("\n\n(Warning: requested file references with line ranges, but the sub-agent did not include them.)");
                    break;
                }
                sub_ui.send_output_update().await;
                agent.append_message(Message::new_user(
                    "Please revise your last answer to include exact file references with line ranges (e.g. `path/to/file.rs:10-20`).",
                ))?;
            }
            Ok::<_, anyhow::Error>(answer)
        };

        // Propagate a parent stop without dropping an already executing child
        // tool. The child's runtime stops new calls and wakes provider/permission
        // waits; existing side effects finish or cooperate with cancellation.
        let propagate_stop = async {
            self.parent_cancellation.cancelled().await;
            child.cancel();
            std::future::pending::<()>().await;
        };
        let result = tokio::select! {
            biased;
            _ = propagate_stop => unreachable!("stop propagation never completes"),
            result = std::panic::AssertUnwindSafe(work).catch_unwind() => {
                result.unwrap_or_else(|_| Err(anyhow::anyhow!("Sub-agent panicked; partial work may have occurred")))
            }
        };
        // Drop handles registration cleanup on every exit, including abort/panic.
        // Explicitly unregister before publishing the terminal child status.
        drop(registration);
        match result {
            Ok(answer)
                if !child.token.is_cancelled() && !self.parent_cancellation.is_cancelled() =>
            {
                sub_ui.set_response(answer.clone());
                sub_ui.send_output_update().await;
                Ok(SubAgentResult {
                    answer,
                    ui_output: sub_ui.get_final_output(),
                })
            }
            result => {
                let cancelled =
                    child.token.is_cancelled() || self.parent_cancellation.is_cancelled();
                let message = if cancelled {
                    "Sub-agent cancelled. Partial work may have occurred; verify side effects before restarting.".to_string()
                } else {
                    format!(
                        "{:#}. Partial work may have occurred; verify side effects before restarting.",
                        result.unwrap_err()
                    )
                };
                if cancelled {
                    sub_ui.set_cancelled();
                }
                sub_ui.set_error(message.clone());
                if cancelled {
                    sub_ui.set_activity(SubAgentActivity::Cancelled);
                }
                sub_ui.send_output_update().await;
                Err(SubAgentFailure {
                    message,
                    ui_output: sub_ui.get_final_output(),
                }
                .into())
            }
        }
    }
}
