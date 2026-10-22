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
        let listing = self.listing(parent_tool_id, &instructions, false);
        let result = self
            .run_child(
                parent_tool_id,
                instructions,
                mode,
                require_file_references,
                &self.parent_cancellation,
                self.ui.clone(),
                listing,
            )
            .await;
        self.unlist(parent_tool_id, self.ui.as_ref()).await;
        result
    }

    async fn start_background(
        &self,
        parent_tool_id: &str,
        instructions: String,
        mode: SubAgentMode,
        require_file_references: bool,
    ) -> Result<BackgroundStart> {
        let Some(background) = self.background.clone() else {
            anyhow::bail!(
                "Background sub-agents are not available here; spawn it without `background`."
            );
        };
        // A writing sub-agent only runs alongside the parent when it gets its
        // own worktree; in a shared tree it must finish inside the turn.
        anyhow::ensure!(
            mode == SubAgentMode::ReadOnly
                || workspace::can_isolate(self.session_config.effective_project_path().map(|p| p.as_path())),
            "Writing sub-agents can only run in the background in a git project; spawn it without `background`."
        );
        let listing = self
            .listing(parent_tool_id, &instructions, true)
            .ok_or_else(|| anyhow::anyhow!("Background sub-agents need a session to report to."))?;
        let agent_id = listing.1;
        let mut started = SubAgentOutput::new();
        started.agent_id = Some(agent_id);
        started.background = true;
        let ui_output = started.to_json();

        let runner = self.clone();
        let tool_id = parent_tool_id.to_string();
        tokio::spawn(async move {
            let result = runner
                .run_child(
                    &tool_id,
                    instructions,
                    mode,
                    require_file_references,
                    &background.cancellation,
                    background.ui.clone(),
                    Some(listing),
                )
                .await;
            runner.unlist(&tool_id, background.ui.as_ref()).await;
            let (outcome, ui_output) = match result {
                Ok(result) => (
                    FinishedOutcome::Completed {
                        answer: result.answer,
                    },
                    result.ui_output,
                ),
                Err(e) => {
                    let (message, ui_output) = match e.downcast_ref::<SubAgentFailure>() {
                        Some(failure) => (failure.message.clone(), failure.ui_output.clone()),
                        None => (format!("{e:#}"), String::new()),
                    };
                    let cancelled = SubAgentOutput::from_json(&ui_output)
                        .is_some_and(|o| o.activity == Some(SubAgentActivity::Cancelled));
                    if cancelled {
                        (FinishedOutcome::Cancelled { message }, ui_output)
                    } else {
                        (FinishedOutcome::Failed { error: message }, ui_output)
                    }
                }
            };
            background
                .sink
                .finished(
                    &background.session_id,
                    FinishedSubAgent {
                        agent_id,
                        tool_id,
                        outcome,
                        ui_output,
                    },
                )
                .await;
        });

        Ok(BackgroundStart {
            agent_id,
            ui_output,
        })
    }
}

impl DefaultSubAgentRunner {
    /// List a starting sub-agent; returns the listing and its id.
    fn listing(
        &self,
        tool_id: &str,
        instructions: &str,
        background: bool,
    ) -> Option<(SubAgentListing, u64)> {
        let (session_id, running) = self.running.as_ref()?;
        let id = running.register(tool_id, instructions, background);
        Some((
            SubAgentListing {
                session_id: session_id.clone(),
                running: running.clone(),
                background,
            },
            id,
        ))
    }

    /// Take an ended sub-agent off the list.
    async fn unlist(&self, tool_id: &str, ui: &dyn UserInterface) {
        let Some((session_id, running)) = &self.running else {
            return;
        };
        running.remove(tool_id);
        let _ = ui
            .send_event(UiEvent::UpdateRunningSubAgents {
                session_id: session_id.clone(),
                agents: running.list(),
            })
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_child(
        &self,
        parent_tool_id: &str,
        instructions: String,
        mode: SubAgentMode,
        require_file_references: bool,
        parent_cancellation: &tools_core::RunCancellation,
        ui: Arc<dyn UserInterface>,
        listing: Option<(SubAgentListing, u64)>,
    ) -> Result<SubAgentResult> {
        // The child's token is cancelled with its parent (the parent run, or
        // the session for background agents) and on its own by the UI; its
        // runtime stops new calls and wakes provider and permission waits. A
        // tool already executing finishes or cooperates.
        let registration = self
            .cancellation_registry
            .register_run(parent_tool_id, parent_cancellation);
        let cancellation = registration.token.clone();
        let (listing, identity) = match listing {
            Some((listing, id)) => {
                let background = listing.background;
                (Some(listing), Some((id, background)))
            }
            None => (None, None),
        };
        let sub_ui = self.build_sub_agent_ui(
            ui,
            parent_tool_id.to_string(),
            cancellation.clone(),
            listing,
        );
        // The sub-agent's conversation becomes a transcript session; without
        // one it still runs.
        let mut transcript = None;
        if let Some((id, background)) = identity {
            sub_ui.set_identity(id, background);
            if let Some(transcripts) = &self.transcripts {
                let title = sub_ui
                    .listing
                    .as_ref()
                    .and_then(|l| l.running.list().into_iter().find(|a| a.id == id))
                    .map(|a| a.title)
                    .unwrap_or_default();
                match transcripts.create(id, &title) {
                    Ok(created) => {
                        sub_ui.set_transcript(created.session_id.clone(), created.ui.clone());
                        transcript = Some((created.session_id, created.persistence, created.ui));
                    }
                    Err(e) => tracing::warn!("No transcript for sub-agent #{id}: {e:#}"),
                }
            }
            sub_ui.send_output_update().await;
            if let Some(listing) = &sub_ui.listing {
                listing.publish(sub_ui.parent.as_ref()).await;
            }
        }

        // The sub-agent's own browsers, listed with the session's while it
        // runs.
        let browsers = Arc::new(web::BrowserSessionManager::default());
        let _listed = self
            .session_browsers
            .as_ref()
            .map(|session| session.register_sub_agent(parent_tool_id, browsers.clone()));

        let transcript_ui = transcript.as_ref().map(|(_, _, ui)| ui.clone());
        let agent_transcript = transcript.map(|(id, persistence, _)| (id, persistence));

        // A writing sub-agent in a git project works in its own worktree, so
        // it can run alongside other writers; its changes are merged back
        // when it finishes. Elsewhere, writing sub-agents take turns.
        let parent_project = self.session_config.effective_project_path().cloned();
        let workspace = match (mode, &parent_project) {
            (SubAgentMode::Default, Some(project)) => {
                match workspace::ChildWorkspace::create(project, parent_tool_id).await {
                    Ok(workspace) => workspace,
                    Err(e) => {
                        tracing::warn!("Sub-agent runs in the shared tree: {e:#}");
                        None
                    }
                }
            }
            _ => None,
        };
        if let Some(workspace) = &workspace {
            let _ = self.sandbox_context.register_root(workspace.root());
        }
        let project_path = workspace
            .as_ref()
            .map(|w| w.project_dir())
            .or_else(|| parent_project.clone());

        let work = async {
            cancellation.check()?;
            let _shared_writer = if mode == SubAgentMode::Default && workspace.is_none() {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(tools_core::Cancelled.into()),
                    guard = workspace::shared_tree_writer(parent_project.as_deref()) => Some(guard),
                }
            } else {
                None
            };
            let mut agent = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(tools_core::Cancelled.into()),
                agent = self.build_agent(parent_tool_id, sub_ui.clone(), self.permission_handler.clone(), browsers.clone(), project_path.clone(), agent_transcript) => agent?,
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
                    &self.model.model_name,
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
        let transcript_id = sub_ui.output.lock().unwrap().session_id.clone();
        if let (Some(ui), Some(session_id)) = (&transcript_ui, transcript_id) {
            let _ = ui
                .send_event(UiEvent::UpdateSessionActivityState {
                    session_id,
                    activity_state: crate::session::instance::SessionActivityState::Idle,
                })
                .await;
        }

        match result {
            Ok(mut answer) if !cancellation.is_cancelled() => {
                if let Some(workspace) = workspace {
                    let location = workspace.root().display().to_string();
                    let report = match workspace.finish().await {
                        Ok(outcome) => outcome.report(),
                        Err(e) => Some(format!(
                            "The sub-agent's changes could not be merged into your working tree: {e:#}. Its worktree is left at {location}."
                        )),
                    };
                    if let Some(report) = report {
                        answer.push_str("\n\n---\n");
                        answer.push_str(&report);
                    }
                }
                sub_ui.set_response(answer.clone());
                sub_ui.send_output_update().await;
                Ok(SubAgentResult {
                    answer,
                    ui_output: sub_ui.get_final_output(),
                })
            }
            result => {
                let cancelled = cancellation.is_cancelled();
                let side_effects = match workspace {
                    Some(workspace) => match workspace.abandon().await {
                        Some(branch) => format!(
                            "Nothing was applied to your working tree; its partial changes are on branch `{branch}`."
                        ),
                        None => "It made no changes.".to_string(),
                    },
                    None => {
                        "Partial work may have occurred; verify side effects before restarting."
                            .to_string()
                    }
                };
                let message = if cancelled {
                    format!("Sub-agent cancelled. {side_effects}")
                } else {
                    match result {
                        Err(e) => format!("{e:#}. {side_effects}"),
                        Ok(_) => format!("Sub-agent cancelled. {side_effects}"),
                    }
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
