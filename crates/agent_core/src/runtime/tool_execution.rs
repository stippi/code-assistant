//! One authorization/invocation/commit pipeline. Scheduling only decides
//! which adjacent invocations may overlap; it does not change their hooks,
//! permission checks, input correction or persistence semantics.
use super::*;
use crate::execution::RuntimeToolOutput;
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use std::time::Instant;
use tools_core::AnyOutput;

struct Invocation {
    request: ToolRequest,
    registry: Arc<ToolRegistry>,
    command_executor: Arc<dyn CommandExecutor>,
    permission_handler: Option<Arc<dyn PermissionMediator>>,
    cancellation: tools_core::RunCancellation,
    session_id: Option<String>,
    services: Box<dyn Any + Send>,
    detached: bool,
}

struct Completion {
    original: ToolRequest,
    execution: ToolExecution,
    services: Option<Box<dyn Any + Send>>,
    started_at: Option<SystemTime>,
    duration: Option<f64>,
    intercepted: bool,
}

impl Completion {
    fn runtime(request: &ToolRequest, output: RuntimeToolOutput) -> Self {
        Self {
            original: request.clone(),
            execution: ToolExecution {
                tool_request: request.clone(),
                result: Box::new(output),
            },
            services: None,
            started_at: None,
            duration: None,
            intercepted: false,
        }
    }
}

impl Invocation {
    async fn run(mut self) -> Completion {
        let original = self.request.clone();
        let started_at = SystemTime::now();
        let start = Instant::now();
        let result = if self.cancellation.is_cancelled() {
            Ok(
                Box::new(RuntimeToolOutput::not_started("The run was cancelled."))
                    as Box<dyn AnyOutput>,
            )
        } else {
            let mut context = ToolContext {
                command_executor: self.command_executor.as_ref(),
                tool_id: Some(self.request.id.clone()),
                session_id: self.session_id,
                permission_handler: self.permission_handler.as_deref(),
                extensions: Some(self.services.as_mut()),
            };
            // The registry is immutable for this run and was checked in prepare.
            let tool = self
                .registry
                .get(&self.request.name)
                .expect("authorized tool");
            match std::panic::AssertUnwindSafe(tool.invoke(&mut context, &mut self.request.input))
                .catch_unwind()
                .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    "Tool panicked; partial side effects may have occurred. Verify the state before retrying."
                )),
            }
        };
        Completion {
            original,
            execution: ToolExecution {
                tool_request: self.request,
                result: result.unwrap_or_else(|error| {
                    Box::new(RuntimeToolOutput::failed(
                        AgentRuntime::format_error_for_user(&error),
                    ))
                }),
            },
            services: if self.detached {
                None
            } else {
                Some(self.services)
            },
            started_at: Some(started_at),
            duration: Some(start.elapsed().as_secs_f64()),
            intercepted: false,
        }
    }
}

impl AgentRuntime {
    pub(super) async fn manage_tool_execution(
        &mut self,
        requests: &[ToolRequest],
    ) -> Result<LoopFlow> {
        let mut seen = std::collections::HashSet::new();
        for request in requests {
            anyhow::ensure!(
                seen.insert(&request.id),
                "Duplicate tool call id: {}",
                request.id
            );
            anyhow::ensure!(
                !self.journal.contains(&request.id),
                "Tool call id has already been recorded: {}",
                request.id
            );
        }
        let mut parallel = vec![false; requests.len()];
        for index in self.hooks.dispatch.parallel_indices(requests) {
            anyhow::ensure!(
                index < requests.len(),
                "Dispatch policy returned invalid index {index}"
            );
            parallel[index] = true;
        }
        let mut blocks: Vec<Option<ContentBlock>> = vec![None; requests.len()];
        let mut index = 0;
        while index < requests.len() && !self.cancellation.is_cancelled() {
            let mut end = index + 1;
            if parallel[index] {
                while end < requests.len() && parallel[end] {
                    end += 1;
                }
            }
            let detached = end - index > 1;
            let mut pending = FuturesUnordered::new();
            for i in index..end {
                if self.cancellation.is_cancelled() {
                    break;
                }
                match self.prepare_invocation(&requests[i], detached).await? {
                    Ok(invocation) => pending.push(async move { (i, invocation.run().await) }),
                    Err(completed) => blocks[i] = Some(self.commit_completion(completed).await?),
                }
            }
            // Consume completion order, not request order. A slow sibling must
            // never keep a completed side effect out of the durable checkpoint.
            // We do not drop active tools on ordinary stop: tools with effects
            // finish or implement their own cooperative cancellation.
            while let Some((i, completed)) = pending.next().await {
                blocks[i] = Some(self.commit_completion(completed).await?);
            }
            index = end;
        }

        for (index, request) in requests.iter().enumerate() {
            if blocks[index].is_none() {
                // This request never got past preparation. Settle the UI too;
                // the model may already have streamed a pending tool card.
                blocks[index] = Some(
                    self.commit_completion(Completion::runtime(
                        request,
                        RuntimeToolOutput::not_started("The run was cancelled before invocation."),
                    ))
                    .await?,
                );
            }
        }
        if !blocks.is_empty() {
            self.append_message(Message::new_user_content(
                blocks.into_iter().flatten().collect(),
            ))?;
        }
        Ok(LoopFlow::Continue)
    }

    async fn authorize_invocation(&self, request: &ToolRequest) -> Result<()> {
        self.cancellation.check()?;
        let tool = self
            .registry
            .get(&request.name)
            .ok_or_else(|| ToolError::UnknownTool(request.name.clone()))?;
        anyhow::ensure!(
            self.registry
                .tool_has_capability(&request.name, &self.tool_capability)
                && !self
                    .excluded_tool_capabilities
                    .iter()
                    .any(|cap| self.registry.tool_has_capability(&request.name, cap)),
            "Tool '{}' is not available in the current scope",
            request.name
        );
        // A stop must not wait for the user to answer a permission prompt;
        // dropping the mediator's future settles the prompt as denied.
        let spec = tool.spec();
        let check = self.permissions.check(
            self.permission_handler.as_deref(),
            &spec,
            Some(&request.id),
            &request.input,
        );
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Err(tools_core::Cancelled.into()),
            result = check => result,
        }
    }

    /// Outer errors are infrastructure failures and abort dispatch. A rejected
    /// or intercepted tool is a normal completion, handled by the same commit path.
    async fn prepare_invocation(
        &mut self,
        request: &ToolRequest,
        detached: bool,
    ) -> Result<std::result::Result<Invocation, Completion>> {
        if let Err(error) = self.authorize_invocation(request).await {
            return Ok(Err(Completion::runtime(
                request,
                RuntimeToolOutput::not_started(Self::format_error_for_user(&error)),
            )));
        }
        // A tool with effects is journaled as started before it runs, so a
        // crash mid-call leaves an "outcome unknown" record instead of
        // nothing. Read-only tools skip this: a missing record means the
        // call did not happen, and repeating it is harmless either way.
        if !self
            .registry
            .tool_has_capability(&request.name, tools_core::spec::capabilities::READ_ONLY)
        {
            self.journal.record(ToolExecution {
                tool_request: request.clone(),
                result: Box::new(RuntimeToolOutput::started()),
            });
            self.checkpoint()?;
        }

        // Interceptors execute on the state owner, even for a parallel group,
        // and only after scope/permission checks and the start checkpoint.
        if let Some(result) = self.intercept_tool(request) {
            let output = result.unwrap_or_else(|error| {
                Box::new(RuntimeToolOutput::failed(Self::format_error_for_user(
                    &error,
                )))
            });
            return Ok(Err(Completion {
                original: request.clone(),
                execution: ToolExecution {
                    tool_request: request.clone(),
                    result: output,
                },
                services: None,
                started_at: Some(SystemTime::now()),
                duration: None,
                intercepted: true,
            }));
        }
        if !self
            .registry
            .is_tool_hidden(&request.name, &self.tool_capability)
        {
            // UI is a projection, not the owner of execution or evidence.
            let _ = self
                .send_ui(AgentUiEvent::UpdateToolStatus {
                    tool_id: request.id.clone(),
                    status: crate::ui::ToolStatus::Running,
                    message: None,
                    output: None,
                    duration_seconds: None,
                    images: vec![],
                })
                .await;
        }
        let services = if detached {
            self.services_provider.detached(&request.id)
        } else {
            self.services_provider
                .begin(self.extensions.as_mut(), &request.id)
        };
        Ok(Ok(Invocation {
            request: request.clone(),
            registry: self.registry.clone(),
            command_executor: self.command_executor.clone(),
            permission_handler: self.permission_handler.clone(),
            cancellation: self.cancellation.clone(),
            session_id: self.session_id.clone(),
            services,
            detached,
        }))
    }

    async fn commit_completion(&mut self, mut completed: Completion) -> Result<ContentBlock> {
        if let Some(services) = completed.services.take() {
            self.services_provider
                .end(self.extensions.as_mut(), services);
        }
        let request = completed.execution.tool_request.clone();
        let success = completed.execution.result.is_success();
        let changed = request.input != completed.original.input;
        let hidden = completed.intercepted
            || self
                .registry
                .is_tool_hidden(&request.name, &self.tool_capability);
        self.journal.record(completed.execution);
        if changed {
            self.update_message_history_with_formatted_tool(&request);
        }
        if success {
            self.after_tool_success(&request);
        }
        // One checkpoint commits the outcome together with everything the
        // hooks derived from it; the UI is only told afterwards.
        self.checkpoint()?;
        let execution = self.journal.find(&request.id).expect("committed outcome");
        let content = execution
            .result
            .as_any()
            .and_then(|out| out.downcast_ref::<RuntimeToolOutput>())
            .map(|out| out.message.clone())
            .unwrap_or_default();
        if !hidden {
            let _ = self
                .send_ui(AgentUiEvent::UpdateToolStatus {
                    tool_id: request.id.clone(),
                    status: if success {
                        crate::ui::ToolStatus::Success
                    } else {
                        crate::ui::ToolStatus::Error
                    },
                    message: Some(execution.result.as_render().status()),
                    output: Some(
                        execution
                            .result
                            .as_render()
                            .render_for_ui(&mut ResourcesTracker::new()),
                    ),
                    duration_seconds: completed.duration,
                    images: execution.result.render_images(),
                })
                .await;
            if changed {
                let _ = self
                    .notify_tool_parameter_updates(
                        &completed.original.input,
                        &request.input,
                        &request.id,
                    )
                    .await;
            }
        }
        Ok(ContentBlock::ToolResult {
            tool_use_id: request.id,
            content: ToolResultContent::text(content),
            is_error: if success { None } else { Some(true) },
            start_time: completed.started_at,
            end_time: Some(SystemTime::now()),
        })
    }
}
