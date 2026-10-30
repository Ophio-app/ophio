// SPDX-License-Identifier: Apache-2.0
use protocol::{ErrorCode, ProtocolError, QueueItemId, model::RunState, queue::*};
use std::collections::{HashMap, HashSet};

pub fn validate_dependencies(
    queue: &QueueView,
    item: QueueItemId,
    deps: &[QueueItemId],
) -> Result<(), ProtocolError> {
    let graph: HashMap<_, _> = queue
        .items
        .iter()
        .map(|i| {
            (
                i.item_id,
                if i.item_id == item {
                    deps
                } else {
                    i.depends_on.as_slice()
                },
            )
        })
        .collect();
    if !graph.contains_key(&item) || deps.iter().any(|d| !graph.contains_key(d)) {
        return Err(ProtocolError::new(
            ErrorCode::NotFound,
            "Queue dependency not found",
        ));
    }
    let mut seen = HashSet::new();
    let mut stack = deps.to_vec();
    while let Some(id) = stack.pop() {
        if id == item {
            return Err(ProtocolError::new(
                ErrorCode::Conflict,
                "Queue dependencies form a cycle",
            ));
        }
        if seen.insert(id)
            && let Some(next) = graph.get(&id)
        {
            stack.extend_from_slice(next);
        }
    }
    Ok(())
}

pub fn dependency_block(
    queue: &QueueView,
    item: &QueueItem,
    outcomes: &HashMap<protocol::RunId, RunState>,
) -> Option<String> {
    for dep in &item.depends_on {
        let Some(parent) = queue.items.iter().find(|i| &i.item_id == dep) else {
            return Some("A prerequisite was removed".into());
        };
        match parent.state {
            QueueItemState::Started { run_id } => match outcomes.get(&run_id) {
                Some(RunState::Completed) => {}
                Some(RunState::Failed | RunState::Interrupted) => {
                    return Some("A prerequisite did not succeed".into());
                }
                Some(RunState::ReconciliationRequired) | None => {
                    return Some("A prerequisite requires reconciliation".into());
                }
                _ => return Some("Waiting for a prerequisite".into()),
            },
            _ => return Some("Waiting for a prerequisite".into()),
        }
    }
    None
}

/// Editing an ordered sequence invalidates all remaining authorizations.
pub fn invalidate(queue: &mut QueueView) {
    queue.revision += 1;
    for (position, item) in queue.items.iter_mut().enumerate() {
        item.position = position as u32;
        if item.is_pending() {
            item.authorization = QueueAuthorization::AskBeforeStart;
            item.authorization_expires_at = None;
        }
    }
}

/// Withdraws the go-ahead of `changed` and of everything that waits on them,
/// directly or through others, and renumbers what is left. The rest keep
/// theirs: their own request, what they wait for and the order of what is
/// still before them are as the person let them start, so taking a task out
/// of Next up or editing one asks again only of what that touches. The
/// caller moves the revision on, carrying the permits kept. Returns the
/// waiting items whose go-ahead went.
pub fn withdraw(queue: &mut QueueView, changed: &[QueueItemId]) -> Vec<QueueItemId> {
    let mut affected: HashSet<QueueItemId> = changed.iter().copied().collect();
    loop {
        let before = affected.len();
        for item in &queue.items {
            if item.depends_on.iter().any(|dep| affected.contains(dep)) {
                affected.insert(item.item_id);
            }
        }
        if affected.len() == before {
            break;
        }
    }
    let mut withdrawn = Vec::new();
    for (position, item) in queue.items.iter_mut().enumerate() {
        item.position = position as u32;
        if item.is_pending() && affected.contains(&item.item_id) {
            item.authorization = QueueAuthorization::AskBeforeStart;
            item.authorization_expires_at = None;
            withdrawn.push(item.item_id);
        }
    }
    withdrawn
}

/// Refuse an order that places pending work ahead of its prerequisites.
pub fn validate_order(queue: &QueueView) -> Result<(), ProtocolError> {
    for (position, item) in queue.items.iter().enumerate() {
        if item.is_pending()
            && item.depends_on.iter().any(|dep| {
                queue
                    .items
                    .iter()
                    .position(|i| i.item_id == *dep)
                    .is_some_and(|p| p >= position)
            })
        {
            return Err(ProtocolError::new(
                ErrorCode::Conflict,
                "Move refused: a task must stay after the tasks it waits for",
            ));
        }
    }
    Ok(())
}

/// The stored limits of a single reviewed start authorization.
#[derive(Debug)]
pub struct StartPermit {
    pub task_revision: u64,
    pub queue_revision: u64,
    pub remaining: u32,
    pub expires_at: Option<protocol::time::Timestamp>,
}

/// Both the item and its one-use permission must still match the reviewed queue.
pub fn permit_valid(
    queue: &QueueView,
    item: &QueueItem,
    task_revision: u64,
    permit: &StartPermit,
    now: protocol::time::Timestamp,
) -> bool {
    !queue.paused
        && item.is_pending()
        && item.authorization == QueueAuthorization::AutoStart
        && permit.remaining == 1
        && task_revision == permit.task_revision
        && queue.revision == permit.queue_revision
        && permit.expires_at.is_some_and(|end| now < end)
        && item.authorization_expires_at == permit.expires_at
}
