//! [`TestCore`]: the real session core for end-to-end tests.

use code_assistant_core::persistence::FileSessionPersistence;
use code_assistant_core::session::event_stream::EventStream;
use code_assistant_core::session::service::{
    AgentRuntimeOptions, LlmClientFactory, SessionService, default_project_manager_factory,
};
use code_assistant_core::session::{SessionConfig, SessionManager};
use std::future::Future;
use std::sync::Arc;
use tempfile::TempDir;

/// A [`SessionService`] with its worker on a tokio runtime of its own, as in
/// the app. Sessions live in a temp dir; agents answer from the injected LLM
/// and run commands against a mock executor.
pub struct TestCore {
    pub service: SessionService,
    runtime: tokio::runtime::Runtime,
    _sessions_dir: TempDir,
}

impl TestCore {
    pub fn new(llm: LlmClientFactory) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let sessions_dir = TempDir::new().expect("sessions dir");
        let events = EventStream::new();
        let manager = Arc::new(tokio::sync::Mutex::new(SessionManager::new(
            FileSessionPersistence::new_with_root_dir(sessions_dir.path().to_path_buf()),
            SessionConfig::default(),
            "test-model".to_string(),
            code_assistant_core::tools::test_registry(),
            events.clone(),
        )));
        let options = AgentRuntimeOptions {
            record_path: None,
            playback_path: None,
            fast_playback: false,
            command_executor_factory: Arc::new(|_| {
                Box::new(code_assistant_core::mocks::create_command_executor_mock())
            }),
            project_manager_factory: default_project_manager_factory(),
            llm_client_factory: Some(llm),
        };
        let (service, worker) = SessionService::new(manager, Arc::new(options), events);
        runtime.spawn(worker);
        Self {
            service,
            runtime,
            _sessions_dir: sessions_dir,
        }
    }

    /// Run a service call to completion, for setting up what a test starts
    /// from (the UI's own calls go through its commands).
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// A new, empty session.
    pub fn create_session(&self) -> String {
        self.block_on(self.service.create_session(None, None))
            .expect("session created")
    }
}
