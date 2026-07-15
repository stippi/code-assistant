//! The production [`GoalEvaluator`]: judge one autonomous goal turn against
//! the goal's completion contract with a bounded model call, mapping a
//! [`TurnOutcome`] to an [`Evaluation`].
//!
//! This is the LLM-shaped seam the bounded controller trusts (see
//! [`crate::goals`]). It is a *judge*, not the working agent: given the
//! contract and the agent's own account of the turn, it decides one verdict —
//! and is deliberately strict about `Satisfied`, which is the only path a goal
//! has to `Done`. Kept parallel to `pal_observations`' `LlmExtractor`: a thin
//! wrapper over the `llm` crate, so any provider configured in the shared
//! `models.json` works. Unlike the extractor it runs in an async context (the
//! controller pass), so it calls the provider directly rather than through a
//! blocking bridge.

use crate::goals::{AttemptVerdict, Evaluation, Goal, GoalEvaluator, TurnOutcome};
use crate::waits::{WaitKind, WaitRequest};
use chrono::NaiveDateTime;
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

const DEFAULT_EVALUATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The judge's discipline: strict about success, honest about obstacles.
const SYSTEM_PROMPT: &str = "\
You are the evaluator of an autonomous assistant that is pursuing a durable \
goal on its user's behalf. You are given the goal's completion contract and \
the assistant's own report of what it did this turn. Judge the turn against \
the contract and return exactly one verdict.

Respond with a JSON object and nothing else:
{\"verdict\": \"progressed\" | \"satisfied\" | \"blocked\" | \"needs_input\" | \"stopped\" | \"waiting\", \
\"summary\": \"...\", \"artifacts\": [\"...\"], \"evidence\": [\"...\"], \
\"completed_subgoals\": [\"exact checklist text\"]}

- verdict:
  - \"satisfied\": the contract's verification is DEMONSTRABLY met by the \
evidence in the report. Be strict — never satisfied merely because the \
assistant says it is done, or because the work looks plausible. If the \
verification was not actually run and shown to pass, it is not satisfied. A \
constraint or boundary violation also rules out satisfaction.
  - \"progressed\": real, concrete progress was made, but the contract is not \
yet fully met.
  - \"blocked\": a genuine external obstacle stopped the work — something the \
assistant cannot clear by itself (a missing document, a failing external \
service, a required credential). Not for ordinary difficulty.
  - \"needs_input\": the work cannot continue without a decision or answer \
from the user.
  - \"stopped\": the completion contract's explicit Stop-if condition has been \
met. Continuing would violate the user's envelope; cite the observed fact in \
the summary.
  - \"waiting\": the turn set up a durable dependency and there is genuinely \
nothing more to do until it resolves — a background build or process must \
finish, a scheduled job or child agent must complete, an external event or a \
reply from the user must arrive, or work should simply resume at a later time. \
Prefer this over \"progressed\" when the next step is only to wait: it parks \
the goal so it burns no turns polling. Do NOT use it to avoid hard work. When \
you use it, include a \"wait\" object naming the barrier.
- wait: only with the \"waiting\" verdict. A JSON object naming exactly one \
barrier (add an optional ISO-8601 \"timeout\" after which the goal should wake \
even if the barrier has not fired):
  - {\"barrier\": \"until\", \"at\": \"2026-07-14T18:00:00\"} — resume at a time.
  - {\"barrier\": \"process_exit\", \"handle\": \"<pty handle>\"} — a background \
process exits.
  - {\"barrier\": \"output_pattern\", \"handle\": \"<pty handle>\", \"pattern\": \
\"BUILD SUCCESSFUL\"} — a background process prints a pattern.
  - {\"barrier\": \"job_completion\", \"job_id\": \"<id>\"} — a scheduled job \
finishes.
  - {\"barrier\": \"sub_agent_completion\", \"agent_id\": \"<id>\"} — a child \
agent finishes.
  - {\"barrier\": \"event\", \"key\": \"<event key>\"} — an external event arrives.
  - {\"barrier\": \"human_input\"} — the user replies on this channel.
- summary: a short, factual account of what the turn did (one or two \
sentences). No speculation, no chain-of-thought.
- artifacts: file paths or references the turn produced or changed, as stated \
in the report. [] if none.
- evidence: the verification output or checks that support your verdict \
(command results, file checks). [] if none.
- completed_subgoals: exact text of checklist entries demonstrably completed \
by this turn's evidence. [] if none.";

/// Frame the contract and the turn's report for the judge. Kept compact and
/// factual — the judge sees the same evidence the ledger will record.
fn user_prompt(goal: &Goal, turn: &TurnOutcome) -> String {
    let c = &goal.contract;
    let mut lines = vec![
        format!("Goal: {}", goal.objective),
        String::new(),
        "Completion contract:".to_string(),
        format!("- Done when: {}", c.outcome),
        format!("- Verify by: {}", c.verification),
        format!("- Stop if: {}", c.stop_condition),
    ];
    for constraint in &c.constraints {
        lines.push(format!("- Constraint: {constraint}"));
    }
    for boundary in &c.boundaries {
        lines.push(format!("- Boundary: {boundary}"));
    }
    if !goal.subgoals.is_empty() {
        lines.push(String::new());
        lines.push("Checklist:".to_string());
        for subgoal in &goal.subgoals {
            lines.push(format!(
                "- [{}] {}",
                if subgoal.done { "x" } else { " " },
                subgoal.description
            ));
        }
    }

    lines.push(String::new());
    lines.push("The assistant's report of this turn:".to_string());
    lines.push(if turn.assistant_summary.trim().is_empty() {
        "(the assistant reported nothing)".to_string()
    } else {
        turn.assistant_summary.trim().to_string()
    });
    if !turn.artifacts.is_empty() {
        lines.push(format!("Artifacts named: {}", turn.artifacts.join(", ")));
    }
    if !turn.verification.is_empty() {
        lines.push(format!(
            "Verification output: {}",
            turn.verification.join("\n")
        ));
    }
    lines.join("\n")
}

#[derive(Deserialize)]
struct EvalResponse {
    verdict: AttemptVerdict,
    summary: String,
    #[serde(default)]
    artifacts: Vec<String>,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    completed_subgoals: Vec<String>,
    #[serde(default)]
    wait: Option<WaitRequestDto>,
}

/// The evaluator's `wait` object: a flat, model-friendly shape (the barrier's
/// fields inline with an optional `timeout`) that lifts into a [`WaitRequest`].
/// [`WaitKind`]'s own `barrier` tag drives which fields are required.
#[derive(Deserialize)]
struct WaitRequestDto {
    #[serde(flatten)]
    kind: WaitKind,
    #[serde(default)]
    timeout: Option<NaiveDateTime>,
}

impl WaitRequestDto {
    fn into_request(self) -> WaitRequest {
        WaitRequest {
            kind: self.kind,
            timeout: self.timeout,
        }
    }
}

/// Pull the JSON object out of a model response that may be wrapped in code
/// fences or chatter — the same tolerance as the observation extractor.
/// Anything between the first `{` and the last `}` is given to the parser,
/// which stays the arbiter of validity.
fn parse_evaluation(text: &str) -> anyhow::Result<Evaluation> {
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        anyhow::bail!("evaluator response contains no JSON object: {text:?}");
    };
    anyhow::ensure!(
        start < end,
        "evaluator response contains no JSON object: {text:?}"
    );
    let parsed: EvalResponse = serde_json::from_str(&text[start..=end])
        .map_err(|e| anyhow::anyhow!("evaluator response is not a valid verdict object: {e}"))?;
    anyhow::ensure!(
        parsed.verdict != AttemptVerdict::Error,
        "evaluator may not emit the controller-only error verdict"
    );
    let summary = parsed.summary.trim().to_string();
    anyhow::ensure!(!summary.is_empty(), "evaluator returned an empty summary");
    Ok(Evaluation {
        verdict: parsed.verdict,
        summary,
        artifacts: clean(parsed.artifacts),
        evidence: clean(parsed.evidence),
        completed_subgoals: clean(parsed.completed_subgoals),
        wait: parsed.wait.map(WaitRequestDto::into_request),
    })
}

fn clean(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Production evaluator: one `send_message` per turn against a model from the
/// shared configuration (built via `llm::factory` in `pal::runtime`).
pub struct LlmGoalEvaluator {
    provider: Mutex<Box<dyn llm::LLMProvider>>,
    request_counter: AtomicU64,
    timeout: std::time::Duration,
}

impl LlmGoalEvaluator {
    pub fn new(provider: Box<dyn llm::LLMProvider>) -> Self {
        Self {
            provider: Mutex::new(provider),
            request_counter: AtomicU64::new(1),
            timeout: DEFAULT_EVALUATION_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait::async_trait]
impl GoalEvaluator for LlmGoalEvaluator {
    async fn evaluate(&self, goal: &Goal, turn: &TurnOutcome) -> anyhow::Result<Evaluation> {
        let request = llm::LLMRequest {
            messages: vec![llm::Message::new_user(user_prompt(goal, turn))],
            system_prompt: SYSTEM_PROMPT.to_string(),
            request_id: self.request_counter.fetch_add(1, Ordering::Relaxed),
            session_id: format!("pal-goal-eval-{}", goal.id),
            ..Default::default()
        };
        let mut provider = self.provider.lock().await;
        let response = tokio::time::timeout(self.timeout, provider.send_message(request, None))
            .await
            .map_err(|_| anyhow::anyhow!("goal evaluation timed out after {:?}", self.timeout))?
            .map_err(|e| anyhow::anyhow!("goal evaluation model call failed: {e:#}"))?;
        let text: String = response
            .content
            .iter()
            .filter_map(|block| match block {
                llm::ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        parse_evaluation(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goals::{Budget, CompletionContract};
    use crate::OwnerKey;
    use chrono::NaiveDate;
    use std::sync::{Arc, Mutex as StdMutex};

    fn goal() -> Goal {
        Goal::new(
            "goal-1",
            OwnerKey::from_parts(&["telegram", "private", "42"]),
            "prepare the 2025 tax return",
            CompletionContract::new(
                "a filled ELSTER draft",
                "the draft file validates",
                "give up if a required document is missing",
            ),
            Budget::turns(5),
            NaiveDate::from_ymd_opt(2026, 7, 14)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        )
    }

    fn outcome(summary: &str) -> TurnOutcome {
        TurnOutcome {
            assistant_summary: summary.to_string(),
            artifacts: Vec::new(),
            verification: Vec::new(),
        }
    }

    /// Replies with a fixed text; records the requests it saw.
    struct ScriptedProvider {
        reply: &'static str,
        fail: bool,
        seen: Arc<StdMutex<Vec<llm::LLMRequest>>>,
    }

    impl ScriptedProvider {
        fn new(reply: &'static str) -> (Self, Arc<StdMutex<Vec<llm::LLMRequest>>>) {
            let seen = Arc::new(StdMutex::new(Vec::new()));
            (
                Self {
                    reply,
                    fail: false,
                    seen: seen.clone(),
                },
                seen,
            )
        }
    }

    #[async_trait::async_trait]
    impl llm::LLMProvider for ScriptedProvider {
        async fn send_message(
            &mut self,
            request: llm::LLMRequest,
            _callback: Option<&llm::StreamingCallback>,
        ) -> anyhow::Result<llm::LLMResponse> {
            self.seen.lock().unwrap().push(request);
            if self.fail {
                anyhow::bail!("model unreachable");
            }
            Ok(llm::LLMResponse {
                content: vec![llm::ContentBlock::Text {
                    text: self.reply.to_string(),
                    start_time: None,
                    end_time: None,
                }],
                usage: llm::Usage::zero(),
                rate_limit_info: None,
            })
        }
    }

    fn evaluator(reply: &'static str) -> (LlmGoalEvaluator, Arc<StdMutex<Vec<llm::LLMRequest>>>) {
        let (provider, seen) = ScriptedProvider::new(reply);
        (LlmGoalEvaluator::new(Box::new(provider)), seen)
    }

    #[tokio::test]
    async fn sends_the_contract_and_the_turn_report() {
        let (evaluator, seen) = evaluator(r#"{"verdict": "progressed", "summary": "did a step"}"#);
        evaluator
            .evaluate(&goal(), &outcome("downloaded the receipts"))
            .await
            .unwrap();

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert!(
            req.system_prompt.contains("JSON object"),
            "{}",
            req.system_prompt
        );
        let llm::MessageContent::Text(prompt) = &req.messages[0].content else {
            panic!("expected a plain text message");
        };
        assert!(prompt.contains("prepare the 2025 tax return"), "{prompt}");
        assert!(prompt.contains("the draft file validates"), "{prompt}");
        assert!(prompt.contains("downloaded the receipts"), "{prompt}");
    }

    #[tokio::test]
    async fn parses_a_clean_verdict_object() {
        let (evaluator, _) = evaluator(
            r#"{"verdict": "satisfied", "summary": "draft validated", "artifacts": ["elster/draft.xml"], "evidence": ["validation passed"]}"#,
        );
        let eval = evaluator
            .evaluate(&goal(), &outcome("ran the check"))
            .await
            .unwrap();
        assert_eq!(eval.verdict, AttemptVerdict::Satisfied);
        assert_eq!(eval.summary, "draft validated");
        assert_eq!(eval.artifacts, ["elster/draft.xml"]);
        assert_eq!(eval.evidence, ["validation passed"]);
    }

    #[tokio::test]
    async fn subgoals_are_judged_and_completed_from_evidence() {
        let (evaluator, seen) = evaluator(
            r#"{"verdict":"progressed","summary":"receipts collected","completed_subgoals":["collect receipts"]}"#,
        );
        let mut goal = goal();
        goal.subgoals = vec![crate::goals::Subgoal::new("collect receipts")];

        let evaluation = evaluator
            .evaluate(&goal, &outcome("downloaded every receipt"))
            .await
            .unwrap();

        assert_eq!(evaluation.completed_subgoals, ["collect receipts"]);
        let requests = seen.lock().unwrap();
        let llm::MessageContent::Text(prompt) = &requests[0].messages[0].content else {
            panic!("expected text prompt");
        };
        assert!(prompt.contains("[ ] collect receipts"), "{prompt}");
    }

    #[tokio::test]
    async fn tolerates_code_fences_and_chatter() {
        let (evaluator, _) = evaluator(
            "Here is my judgement:\n```json\n{\n  \"verdict\": \"blocked\",\n  \"summary\": \"the 2024 statement is missing\"\n}\n```\n",
        );
        let eval = evaluator
            .evaluate(&goal(), &outcome("looked for the statement"))
            .await
            .unwrap();
        assert_eq!(eval.verdict, AttemptVerdict::Blocked);
        assert_eq!(eval.summary, "the 2024 statement is missing");
        assert!(eval.artifacts.is_empty());
    }

    #[tokio::test]
    async fn parses_a_waiting_verdict_with_a_flat_barrier() {
        let (evaluator, _) = evaluator(
            r#"{"verdict":"waiting","summary":"started the build","wait":{"barrier":"output_pattern","handle":"pty-3","pattern":"BUILD SUCCESSFUL","timeout":"2026-07-14T18:00:00"}}"#,
        );
        let eval = evaluator
            .evaluate(&goal(), &outcome("kicked off the build in the background"))
            .await
            .unwrap();
        assert_eq!(eval.verdict, AttemptVerdict::Waiting);
        let request = eval.wait.expect("a waiting verdict carries a barrier");
        assert_eq!(
            request.kind,
            WaitKind::OutputPattern {
                handle: "pty-3".into(),
                pattern: "BUILD SUCCESSFUL".into(),
            }
        );
        assert_eq!(
            request.timeout,
            Some(
                NaiveDateTime::parse_from_str("2026-07-14 18:00:00", "%Y-%m-%d %H:%M:%S").unwrap()
            )
        );
    }

    #[tokio::test]
    async fn a_human_input_barrier_needs_no_extra_fields() {
        let (evaluator, _) = evaluator(
            r#"{"verdict":"waiting","summary":"asked the user to confirm the account","wait":{"barrier":"human_input"}}"#,
        );
        let eval = evaluator
            .evaluate(&goal(), &outcome("posed the question"))
            .await
            .unwrap();
        assert_eq!(eval.verdict, AttemptVerdict::Waiting);
        let request = eval.wait.expect("a waiting verdict carries a barrier");
        assert_eq!(request.kind, WaitKind::HumanInput);
        assert!(request.timeout.is_none());
    }

    #[tokio::test]
    async fn needs_input_verdict_maps_through() {
        let (evaluator, _) =
            evaluator(r#"{"verdict": "needs_input", "summary": "which bank account?"}"#);
        let eval = evaluator
            .evaluate(&goal(), &outcome("hit a fork"))
            .await
            .unwrap();
        assert_eq!(eval.verdict, AttemptVerdict::NeedsInput);
    }

    #[tokio::test]
    async fn stop_condition_verdict_maps_through() {
        let (evaluator, _) = evaluator(
            r#"{"verdict": "stopped", "summary": "the required document is unavailable"}"#,
        );
        let eval = evaluator
            .evaluate(&goal(), &outcome("confirmed the document is unavailable"))
            .await
            .unwrap();
        assert_eq!(eval.verdict, AttemptVerdict::Stopped);
    }

    #[tokio::test]
    async fn a_response_without_a_json_object_is_an_error() {
        let (evaluator, _) = evaluator("I think it made good progress this turn.");
        let err = evaluator
            .evaluate(&goal(), &outcome("x"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no JSON object"), "{err}");
    }

    #[tokio::test]
    async fn an_unknown_verdict_is_an_error() {
        let (evaluator, _) = evaluator(r#"{"verdict": "maybe", "summary": "unsure"}"#);
        let err = evaluator
            .evaluate(&goal(), &outcome("x"))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not a valid verdict object"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_empty_summary_is_an_error() {
        let (evaluator, _) = evaluator(r#"{"verdict": "progressed", "summary": "   "}"#);
        let err = evaluator
            .evaluate(&goal(), &outcome("x"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty summary"), "{err}");
    }

    #[tokio::test]
    async fn a_failing_model_call_surfaces_as_an_error() {
        let (mut provider, _) = ScriptedProvider::new("{}");
        provider.fail = true;
        let evaluator = LlmGoalEvaluator::new(Box::new(provider));
        let err = evaluator
            .evaluate(&goal(), &outcome("x"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("model call failed"), "{err}");
    }

    #[tokio::test]
    async fn a_wedged_model_call_is_time_bounded() {
        struct WedgedProvider;

        #[async_trait::async_trait]
        impl llm::LLMProvider for WedgedProvider {
            async fn send_message(
                &mut self,
                _request: llm::LLMRequest,
                _callback: Option<&llm::StreamingCallback>,
            ) -> anyhow::Result<llm::LLMResponse> {
                std::future::pending().await
            }
        }

        let evaluator = LlmGoalEvaluator::new(Box::new(WedgedProvider))
            .with_timeout(std::time::Duration::from_millis(10));
        let err = evaluator
            .evaluate(&goal(), &outcome("x"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
    }
}
