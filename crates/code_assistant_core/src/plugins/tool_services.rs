//! Provides code-assistant's [`ToolServices`] bundle to each tool
//! invocation. The plan state moves into the services for the duration of a
//! sequential invocation and is taken back afterwards; detached (parallel)
//! invocations run without plan access.

use agent_core::hooks::ToolServicesProvider;
use crate::agent::SubAgentRunner;
use crate::config::ProjectManager;
use crate::plugins::AgentAppState;
use crate::tools::ToolServices;
use crate::ui::UserInterface;
use std::any::Any;
use std::sync::Arc;

pub struct CodeAssistantToolServices {
    pub project_manager: Arc<dyn ProjectManager>,
    pub ui: Arc<dyn UserInterface>,
    pub sub_agent_runner: Option<Arc<dyn SubAgentRunner>>,
}

impl ToolServicesProvider for CodeAssistantToolServices {
    fn begin(&self, loop_ext: &mut (dyn Any + Send), _tool_id: &str) -> Box<dyn Any + Send> {
        let plan = std::mem::take(&mut AgentAppState::of(loop_ext).plan);
        Box::new(ToolServices {
            project_manager: self.project_manager.clone(),
            plan: Some(plan),
            ui: Some(self.ui.clone()),
            sub_agent_runner: self.sub_agent_runner.clone(),
        })
    }

    fn end(&self, loop_ext: &mut (dyn Any + Send), mut services: Box<dyn Any + Send>) {
        if let Some(services) = services.downcast_mut::<ToolServices>() {
            AgentAppState::of(loop_ext).plan = services.plan.take().unwrap_or_default();
        }
    }

    fn detached(&self, _tool_id: &str) -> Box<dyn Any + Send> {
        Box::new(ToolServices {
            project_manager: self.project_manager.clone(),
            plan: None, // No plan access in detached execution
            ui: Some(self.ui.clone()),
            sub_agent_runner: self.sub_agent_runner.clone(),
        })
    }
}
