use super::dispatch_tests::{fixture, request};
use super::*;
use std::collections::BTreeSet;
use std::time::Duration;

/// Withdraws whatever the test puts into the shared set, at any time.
#[derive(Clone, Default)]
struct Withdrawals(Arc<Mutex<BTreeSet<String>>>);

impl Withdrawals {
    fn withdraw(&self, name: &str) {
        self.0.lock().unwrap().insert(name.to_string());
    }
}

impl crate::tool_offer::ToolAvailability for Withdrawals {
    fn withdrawn(&self, _: &(dyn Any + Send)) -> BTreeSet<String> {
        self.0.lock().unwrap().clone()
    }
}

fn offered_names(agent: &AgentRuntime) -> Vec<String> {
    agent
        .offered_tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect()
}

/// An assistant response recorded `age` ago.
fn answered(agent: &mut AgentRuntime, age: Duration) {
    agent
        .append_message(Message::new_assistant("done"))
        .unwrap();
    agent
        .conversation
        .last_assistant_node_mut()
        .unwrap()
        .created_at = SystemTime::now() - age;
}

#[tokio::test]
async fn a_tool_withdrawn_during_the_run_is_refused_but_stays_offered() {
    let requests = vec![request("one", false)];
    let mut f = fixture(&requests);
    let withdrawals = Withdrawals::default();
    f.agent.hooks.tool_availability = Box::new(withdrawals.clone());
    f.agent.settle_tool_offer();
    withdrawals.withdraw("probe");

    assert_eq!(offered_names(&f.agent), vec!["probe"]);
    f.agent.manage_tool_execution(&requests).await.unwrap();
    assert!(f.calls.lock().unwrap().is_empty(), "must not be invoked");
    let saved = f.saved.saved();
    let rendered = saved.executions[0]
        .result
        .as_render()
        .render(&mut ResourcesTracker::new());
    assert!(
        rendered.contains("'probe' has been disabled"),
        "unexpected result: {rendered}"
    );
}

#[tokio::test]
async fn the_next_run_keeps_offering_a_withdrawn_tool_while_the_cache_is_warm() {
    let requests = vec![request("one", false)];
    let mut f = fixture(&requests);
    let withdrawals = Withdrawals::default();
    f.agent.hooks.tool_availability = Box::new(withdrawals.clone());
    answered(&mut f.agent, Duration::from_secs(60));
    withdrawals.withdraw("probe");

    f.agent.settle_tool_offer();
    assert_eq!(offered_names(&f.agent), vec!["probe"]);
}

#[tokio::test]
async fn the_next_run_drops_a_withdrawn_tool_once_the_cache_expired() {
    let requests = vec![request("one", false)];
    let mut f = fixture(&requests);
    let withdrawals = Withdrawals::default();
    f.agent.hooks.tool_availability = Box::new(withdrawals.clone());
    answered(&mut f.agent, Duration::from_secs(600));
    withdrawals.withdraw("probe");

    f.agent.settle_tool_offer();
    assert!(offered_names(&f.agent).is_empty());
    f.agent.checkpoint().unwrap();
    assert_eq!(
        f.saved.saved().withheld_tools,
        BTreeSet::from(["probe".to_string()])
    );

    // A call of it anyway is refused like any withdrawn tool.
    f.agent.manage_tool_execution(&requests).await.unwrap();
    assert!(f.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_restored_offer_survives_a_warm_cache() {
    let requests = vec![request("one", false)];
    let mut f = fixture(&requests);
    f.agent
        .set_withheld_tools(BTreeSet::from(["probe".to_string()]));
    answered(&mut f.agent, Duration::from_secs(60));

    // The application wants it back, but the cache still holds the offer
    // without it.
    f.agent.settle_tool_offer();
    assert!(offered_names(&f.agent).is_empty());
}
