//! Deferred RPM notify/remove — off EffectModule hook stack.

use std::collections::VecDeque;

use parking_lot::Mutex;

// Sends are queued to the async simple_server sender thread, so the per-job cost here is only
// tracker lookup + serialize; 8/tick starved heavy scenes (effects trickled into RPM or never
// showed). 64 clears realistic bursts within a frame or two.
const MAX_PER_TICK: usize = 64;

struct Pending {
    notifies: VecDeque<u64>,
    removes: VecDeque<(u64, bool)>,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    notifies: VecDeque::new(),
    removes: VecDeque::new(),
});

pub fn queue_notify(id: u64) {
    PENDING.lock().notifies.push_back(id);
}

pub fn queue_remove(id: u64, notified: bool) {
    PENDING.lock().removes.push_back((id, notified));
}

pub fn process() {
    let mut drained = 0u64;
    for _ in 0..MAX_PER_TICK {
        let job = {
            let mut p = PENDING.lock();
            if let Some(id) = p.notifies.pop_front() {
                Some(Job::Notify(id))
            } else if let Some(r) = p.removes.pop_front() {
                Some(Job::Remove(r))
            } else {
                None
            }
        };
        let Some(job) = job else { break };
        drained += 1;
        match job {
            Job::Notify(id) => flush_notify(id),
            Job::Remove((id, n)) => crate::slight::effect_viewer::show::hide_effect(id, n),
        }
    }
    if drained > 0 {
        crate::slight::diag::note_flush(drained);
    }
}

/// Jobs currently queued (diagnostic — persistent growth = flush starvation).
pub fn depth() -> usize {
    let p = PENDING.lock();
    p.notifies.len() + p.removes.len()
}

enum Job {
    Notify(u64),
    Remove((u64, bool)),
}

fn flush_notify(id: u64) {
    // `id` is the effect KIND hash — the stable RPM object id (one tab per kind).
    let Some((name, data)) = crate::slight::effect_viewer::kinds::for_notify(id) else {
        return;
    };
    crate::slight::effect_viewer::show::show_effect(id, &name, &data);
}
