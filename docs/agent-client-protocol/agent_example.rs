//! A minimal ACP agent server for educational purposes — **current SDK (0.14.x)**.
//!
//! This mirrors the upstream `simple_agent.rs` example for the redesigned
//! Role/Component SDK. The previous version of this file targeted the old
//! `0.9.x` `AgentSideConnection` + `impl acp::Agent` API, which no longer
//! exists (removed in `0.11.0`).
//!
//! The agent communicates with clients over stdio. Handlers are registered on a
//! connection builder; there is no `Agent` trait to implement.
//!
//! Upstream examples:
//! - simple_agent.rs:  <https://github.com/agentclientprotocol/rust-sdk/blob/main/src/agent-client-protocol/examples/simple_agent.rs>
//! - yolo_one_shot_client.rs (client side)
//!
//! See `crates/ui_acp` in this repository for a full agent wired to the
//! code-assistant backend (sessions, streaming, tools, model selection via
//! session config options, usage/title reporting).

use agent_client_protocol::schema::{
    AgentCapabilities, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, SessionId, StopReason,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Dispatch, Result, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // Shared state captured into handlers via `Arc`.
    let next_session_id = Arc::new(AtomicU64::new(0));

    Agent
        .builder()
        .name("example-agent") // for debugging/tracing
        // --- initialize ---------------------------------------------------
        .on_receive_request(
            async move |req: InitializeRequest, responder, _cx: ConnectionTo<Client>| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        // --- session/new --------------------------------------------------
        .on_receive_request(
            {
                let next_session_id = next_session_id.clone();
                async move |_req: NewSessionRequest, responder, _cx: ConnectionTo<Client>| {
                    let id = next_session_id.fetch_add(1, Ordering::Relaxed);
                    responder.respond(NewSessionResponse::new(SessionId::new(id.to_string())))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // --- session/prompt -----------------------------------------------
        //
        // For real agents, spawn the long-running work and move the `responder`
        // into the task so the dispatch loop stays free to handle
        // `session/cancel`:
        //
        //     tokio::spawn(async move {
        //         let result = run_turn(cx, req).await;
        //         let _ = responder.respond_with_result(result);
        //     });
        //     Ok(())
        .on_receive_request(
            async move |_req: PromptRequest, responder, _cx: ConnectionTo<Client>| {
                responder.respond(PromptResponse::new(StopReason::EndTurn))
            },
            agent_client_protocol::on_receive_request!(),
        )
        // --- fallback: error on any other message -------------------------
        .on_receive_dispatch(
            async move |message: Dispatch, cx: ConnectionTo<Client>| {
                message.respond_with_error(
                    agent_client_protocol::util::internal_error("unhandled message"),
                    cx,
                )
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}
