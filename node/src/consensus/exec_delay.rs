//! Test builds: an executor that falls behind (docs/PROGRAMS-V0.md §10.2, scenario 3).
//!
//! A simulated node whose `Chain::exec_delay_ms` is set holds each finalized block, and the
//! acknowledgement marshal waits for, that many simulated milliseconds before executing it, as a
//! node whose execution is slow does (the deterministic runtime's clock only moves when a task
//! sleeps, so executing a block takes no simulated time of its own). Marshal delivers one block at
//! a time (`max_pending_acks` is 1), so nothing after a held block executes before it, and its
//! tips go on arriving. Setting the delay back to 0 releases what's held at once, in order.
//!
//! Nothing here is built outside tests, and a node whose delay is 0 holds nothing, so the reporter
//! is exactly what production runs.

use super::*;
use std::collections::VecDeque;

/// The blocks an [`Executor`] holds back, shared by its clones.
#[derive(Clone, Default)]
pub struct Delay {
    held: Arc<Mutex<VecDeque<Held>>>,
    /// Set on the clone that executes a released block: it isn't held again.
    releasing: bool,
}

struct Held {
    /// When the release task first saw it (simulated milliseconds): its delay counts from there.
    arrived: Option<u64>,
    update: Update<CBlock>,
}

impl Executor {
    /// `activity`, if this node executes it now: `None` if it's held back instead (a finalized
    /// block while the node is told to execute late, or while earlier ones are still held).
    pub(super) fn hold(&self, activity: Update<CBlock>) -> Option<Update<CBlock>> {
        if self.delay.releasing || !matches!(activity, Update::Block(..)) {
            return Some(activity);
        }
        let mut held = self.delay.held.lock();
        if held.is_empty() && self.chain.lock().exec_delay_ms == 0 {
            return Some(activity);
        }
        held.push_back(Held { arrived: None, update: activity });
        None
    }

    /// Executes each held block once its delay has passed, oldest first: the node's task, which
    /// runs until the node stops. It holds a clone of the executor, whose epoch channel the node's
    /// main task waits on, so it must not outlive the executor marshal holds: when that one is
    /// dropped (a halted node, whose marshal stops) the held blocks' queue has no other owner, and
    /// this returns.
    pub(super) async fn release_when_due<E: Clock>(self, ctx: E) {
        loop {
            ctx.sleep(Duration::from_millis(10)).await;
            if Arc::strong_count(&self.delay.held) < 2 {
                return;
            }
            let now = ms(ctx.current());
            loop {
                let delay = self.chain.lock().exec_delay_ms;
                let due = {
                    let mut held = self.delay.held.lock();
                    let Some(front) = held.front_mut() else { break };
                    let arrived = *front.arrived.get_or_insert(now);
                    if now < arrived.saturating_add(delay) {
                        break;
                    }
                    held.pop_front().map(|h| h.update)
                };
                let Some(update) = due else { break };
                let mut executor = self.clone();
                executor.delay.releasing = true;
                executor.report(update);
                // The next block comes once this one is acknowledged: it's held (or not) when it does.
            }
        }
    }
}
