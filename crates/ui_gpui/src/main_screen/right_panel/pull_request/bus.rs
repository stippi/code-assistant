//! The core's pull request announcements, for the views showing pull
//! requests. The event loop reports them here ([`report`]); views subscribe
//! to [`bus`] and read again what changed.

use code_assistant_core::pull_requests::PullRequestPart;
use github::{PullRequestRef, RepoRef};
use gpui_kit::{App, AppContext, Entity, EventEmitter, Global};

#[derive(Clone, Debug, PartialEq)]
pub enum PullRequestEvent {
    /// A part of a pull request was refreshed or invalidated by a write.
    Changed {
        pr: PullRequestRef,
        part: PullRequestPart,
    },
    /// The cached lists of a repository changed.
    ListChanged { repo: RepoRef },
}

pub struct PullRequestBus;

impl EventEmitter<PullRequestEvent> for PullRequestBus {}

struct PullRequestBusGlobal(Entity<PullRequestBus>);

impl Global for PullRequestBusGlobal {}

/// The bus, created with its first subscriber.
pub fn bus(cx: &mut App) -> Entity<PullRequestBus> {
    if let Some(global) = cx.try_global::<PullRequestBusGlobal>() {
        return global.0.clone();
    }
    let bus = cx.new(|_| PullRequestBus);
    cx.set_global(PullRequestBusGlobal(bus.clone()));
    bus
}

/// Tell the views about `event`; nothing listens before a view subscribed.
pub fn report(event: PullRequestEvent, cx: &mut App) {
    if let Some(bus) = cx.try_global::<PullRequestBusGlobal>().map(|g| g.0.clone()) {
        bus.update(cx, |_, cx| cx.emit(event));
    }
}
