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
        // The child's token is cancelled with the parent run and on its own
        // by the UI; its runtime stops new calls and wakes provider and
        // permission waits. A tool already executing finishes or cooperates.
        let registration = self
            .cancellation_registry
            .register_run(parent_tool_id, &self.parent_cancellation);
        let cancellation = registration.token.clone();
        let sub_ui = self.build_sub_agent_ui(
            self.ui.clone(),
            parent_tool_id.to_string(),
            cancellation.clone(),
        );

        let work = async {
            cancellation.check()?;
            let mut agent = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(tools_core::Cancelled.into()),
                agent = self.build_agent(parent_tool_id, sub_ui.clone(), self.permission_handler.clone()) => agent?,
            };
            agent.set_cancellation(cancellation.clone());
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
                cancellation.check()?;
                let iteration = agent.run_single_iteration().await;
                // Earlier requests and tools can have completed before a later
                // request fails. Preserve their usage as well as their tool list.
                sub_ui.set_usage(compute_sub_agent_usage(
                    &agent.message_history(),
                    &self.model_name,
                ));
                iteration?;
                cancellation.check()?;
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

        let result = std::panic::AssertUnwindSafe(work)
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "Sub-agent panicked; partial work may have occurred"
                ))
            });
        // Unregister before publishing the terminal child status.
        drop(registration);

        match result {
            Ok(answer) if !cancellation.is_cancelled() => {
                sub_ui.set_response(answer.clone());
                sub_ui.send_output_update().await;
                Ok(SubAgentResult {
                    answer,
                    ui_output: sub_ui.get_final_output(),
                })
            }
            result => {
                let cancelled = cancellation.is_cancelled();
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
