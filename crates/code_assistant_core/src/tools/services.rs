//! Code-assistant's application services, handed to tools through
//! [`ToolContext::extensions`].
//!
//! The core [`ToolContext`] stays application-agnostic; everything
//! code-assistant-specific that tools need (project manager, UI, plan state,
//! sub-agent runner) lives here. Tools access it through the
//! [`ToolServicesAccess`] accessors, which perform the single downcast.

use crate::agent::SubAgentRunner;
use crate::config::ProjectManager;
use crate::tools::core::ToolContext;
use crate::types::PlanState;
use crate::ui::UserInterface;
use std::sync::Arc;

/// The services code-assistant provides to its tools.
///
/// Handles are owned (`Arc`) because the struct travels type-erased through
/// `ToolContext::extensions`, which requires a `'static` type.
pub struct ToolServices {
    /// Project manager for accessing files
    pub project_manager: Arc<dyn ProjectManager>,
    /// Plan state for plan-related tools. The agent moves its plan state in
    /// for the duration of a tool invocation and takes it back afterwards.
    pub plan: Option<PlanState>,
    /// Optional UI instance for streaming output and resource events
    pub ui: Option<Arc<dyn UserInterface>>,
    /// Optional sub-agent runner used by the `spawn_agent` tool
    pub sub_agent_runner: Option<Arc<dyn SubAgentRunner>>,
}

impl ToolServices {
    pub fn new(project_manager: Arc<dyn ProjectManager>) -> Self {
        Self {
            project_manager,
            plan: None,
            ui: None,
            sub_agent_runner: None,
        }
    }
}

/// Accessors for [`ToolServices`] on the application-agnostic [`ToolContext`].
pub trait ToolServicesAccess {
    /// The services bundle. Panics when the context was built without one —
    /// all code-assistant call sites provide it.
    fn services(&self) -> &ToolServices;
    fn services_mut(&mut self) -> &mut ToolServices;

    fn project_manager(&self) -> &dyn ProjectManager;
    fn ui(&self) -> Option<&dyn UserInterface>;
    fn sub_agent_runner(&self) -> Option<&dyn SubAgentRunner>;
}

impl ToolServicesAccess for ToolContext<'_> {
    fn services(&self) -> &ToolServices {
        self.extension::<ToolServices>()
            .expect("ToolContext is missing code-assistant's ToolServices")
    }

    fn services_mut(&mut self) -> &mut ToolServices {
        self.extension_mut::<ToolServices>()
            .expect("ToolContext is missing code-assistant's ToolServices")
    }

    fn project_manager(&self) -> &dyn ProjectManager {
        self.services().project_manager.as_ref()
    }

    fn ui(&self) -> Option<&dyn UserInterface> {
        self.extension::<ToolServices>()
            .and_then(|services| services.ui.as_deref())
    }

    fn sub_agent_runner(&self) -> Option<&dyn SubAgentRunner> {
        self.extension::<ToolServices>()
            .and_then(|services| services.sub_agent_runner.as_deref())
    }
}
