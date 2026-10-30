// SPDX-License-Identifier: Apache-2.0
//! The tasks a phone holds. A snapshot carries everything live, then finished
//! tasks newest first as far as one control frame allows; older ones are read
//! in pages as the person reaches them. Runs are carried without the diffs
//! their agents reported, the bulk of a run's size, which a run read whole
//! includes. A long task carries its latest runs; its conversation names the
//! earlier ones, read by id. Pending approvals too large to share a reply with
//! everything else are named instead, and each is read alone, by a phone that
//! says it can. An earlier phone gets every pending approval in its snapshot,
//! or is told to update when they will not fit one frame. Projects, live
//! tasks and Next up cannot wait for a page, so what a person adds is
//! refused when it would leave them too large to load.
use protocol::{ErrorCode, QueryData, auth::Grants, model::*, queue::QueueView, *};
use std::collections::{HashMap, HashSet};

/// What a snapshot may take up, well under the 1 MiB control frame.
const SNAPSHOT_BYTES: usize = 768 * 1024;
/// What a snapshot for a phone that cannot read withheld approvals may take
/// up: one control frame, less room for the reply around it.
const WHOLE_SNAPSHOT_BYTES: usize = 1024 * 1024 - 4 * 1024;
/// What one page of task history may take up.
const PAGE_BYTES: usize = 768 * 1024;
/// What one task's runs may take up in a snapshot or page.
const TASK_RUNS_BYTES: usize = 256 * 1024;
/// What a snapshot's live tasks may take up once a person adds to them,
/// pending approvals aside: those can be withheld. What is left of the
/// snapshot's budget holds everything else it carries.
pub(crate) const LIVE_BYTES: usize = 512 * 1024;
/// What Next up may take up once a person adds to it, well under a frame:
/// it travels whole, waiting requests included.
const QUEUE_BYTES: usize = 768 * 1024;
/// What naming withheld approvals adds besides their ids: the field's key,
/// quotes, brackets and separator, with room to spare.
const WITHHELD_KEY: usize = 32;

/// The requested tasks with the records that belong to them.
struct Records {
    tasks: Vec<Task>,
    runs: HashMap<TaskId, Vec<Run>>,
    approvals: HashMap<TaskId, Vec<Approval>>,
    attention: HashMap<TaskId, Vec<AttentionItem>>,
    /// Attention about no task the host holds.
    unattached: Vec<AttentionItem>,
}

fn cursor(task: &Task) -> TaskCursor {
    TaskCursor {
        updated_at: task.updated_at,
        task_id: task.task_id,
    }
}

impl Records {
    fn for_tasks(
        r: &store::Read<'_>,
        tasks: Vec<Task>,
        live_only: bool,
        include_unattached: bool,
    ) -> store::Result<Self> {
        let mut runs = HashMap::new();
        let mut approvals = HashMap::new();
        let mut attention = HashMap::new();
        for task in &tasks {
            runs.insert(task.task_id, r.history_runs(task, live_only)?);
            let pending = r.history_approvals(task.task_id, live_only)?;
            attention.insert(
                task.task_id,
                if live_only {
                    r.history_live_attention(task.task_id, &pending)?
                } else {
                    r.task_attention(task.task_id)?
                },
            );
            approvals.insert(task.task_id, pending);
        }
        Ok(Self {
            tasks,
            runs,
            approvals,
            attention,
            unattached: if include_unattached {
                r.history_unattached()?
            } else {
                Vec::new()
            },
        })
    }
    fn load_live(r: &store::Read<'_>) -> store::Result<Self> {
        let mut ids: HashSet<TaskId> = r.history_live_ids()?.into_iter().collect();
        ids.extend(queued(r)?);
        let mut tasks = Vec::new();
        for id in ids {
            if let Some(task) = r.get("task", id)? {
                tasks.push(task);
            }
        }
        tasks.sort_by_key(|t| std::cmp::Reverse(cursor(t)));
        Self::for_tasks(r, tasks, true, true)
    }
    fn runs(&self, task: &Task) -> &[Run] {
        self.runs.get(&task.task_id).map_or(&[], Vec::as_slice)
    }
    fn approvals(&self, task: &Task) -> &[Approval] {
        self.approvals.get(&task.task_id).map_or(&[], Vec::as_slice)
    }
    fn attention(&self, task: &Task) -> &[AttentionItem] {
        self.attention.get(&task.task_id).map_or(&[], Vec::as_slice)
    }
    /// The task with the records a caller with `g` may read: every run still
    /// open and its current one, its other runs newest first while they fit,
    /// and the approvals and attention about those runs. Pending approvals
    /// and unresolved attention always come; a reply too full for a pending
    /// approval names it instead.
    fn records(&self, task: &Task, g: &Grants) -> TaskRecords {
        let mut runs: Vec<Run> = self.runs(task).iter().cloned().map(listed).collect();
        runs.sort_by_key(|r| std::cmp::Reverse((r.admitted_at, r.run_id)));
        let (mut used, mut full) = (0usize, false);
        runs.retain(|run| {
            let bytes = size(run).saturating_add(1);
            let kept = open(run)
                || task.current_run == Some(run.run_id)
                || (!full && used.saturating_add(bytes) <= TASK_RUNS_BYTES);
            full |= !kept;
            if kept {
                used = used.saturating_add(bytes);
            }
            kept
        });
        let carried: HashSet<RunId> = runs.iter().map(|r| r.run_id).collect();
        let mut records = empty();
        if g.approvals {
            records.approvals = (self.approvals(task).iter())
                .filter(|a| a.state == ApprovalState::Pending || carried.contains(&a.run_id))
                .cloned()
                .collect();
        }
        if g.tasks {
            records.tasks.push(task.clone());
            records.runs = runs;
            records.attention = (self.attention(task).iter())
                .filter(|a| {
                    a.resolved_at.is_none() || a.run_id.is_none_or(|r| carried.contains(&r))
                })
                .cloned()
                .collect();
        }
        records
    }
    /// Split a known live task into required records and history that shares
    /// the snapshot's remaining budget with finished tasks.
    fn split_live(&self, task: &Task, g: &Grants) -> (TaskRecords, TaskRecords) {
        let mut live = self.records(task, g);
        let (runs, earlier) = std::mem::take(&mut live.runs)
            .into_iter()
            .partition(|r| open(r) || task.current_run == Some(r.run_id));
        live.runs = runs;
        let (approvals, settled) = std::mem::take(&mut live.approvals)
            .into_iter()
            .partition(|a| a.state == ApprovalState::Pending);
        let (attention, resolved) = std::mem::take(&mut live.attention)
            .into_iter()
            .partition(|a| a.resolved_at.is_none());
        live.approvals = approvals;
        live.attention = attention;
        let rest = TaskRecords {
            tasks: Vec::new(),
            runs: earlier,
            approvals: settled,
            attention: resolved,
            withheld_approvals: Vec::new(),
        };
        (live, rest)
    }
    /// The task as a page carries it, and its weight. One whose decided
    /// approvals would take it past a page is sent without them, and then
    /// with only the pending approvals that fit, naming the rest.
    fn whole(&self, task: &Task, g: &Grants) -> (TaskRecords, usize) {
        let mut records = self.records(task, g);
        let bytes = weight(&records);
        if bytes <= PAGE_BYTES {
            return (records, bytes);
        }
        records
            .approvals
            .retain(|a| a.state == ApprovalState::Pending);
        let bytes = weight(&records);
        if bytes <= PAGE_BYTES {
            return (records, bytes);
        }
        let pending = std::mem::take(&mut records.approvals);
        let mut used = weight(&records);
        (records.approvals, records.withheld_approvals) = within(pending, &mut used, PAGE_BYTES);
        let bytes = weight(&records);
        (records, bytes)
    }
}

fn queued(r: &store::Read<'_>) -> store::Result<HashSet<TaskId>> {
    Ok(r.list::<QueueView>("queue")?
        .into_iter()
        .flat_map(|q| q.items.into_iter())
        .filter(|i| i.is_pending())
        .map(|i| i.task_id)
        .collect())
}

/// A run a person may still stop: its turn is going, or what it started is
/// still running or not yet confirmed ended.
fn open(run: &Run) -> bool {
    run.ended_at.is_none() || run.holds_writer || run.cleanup_pending
}

/// A run as lists carry it: without the diffs its agent reported, which
/// `Query::Run` reads, and with its helpers bounded if it was recorded before
/// they were.
fn listed(mut run: Run) -> Run {
    if let Some(outcome) = &mut run.outcome {
        for file in &mut outcome.changed_files {
            file.diff = None;
        }
    }
    crate::children::list_recent(&mut run);
    run
}

fn empty() -> TaskRecords {
    TaskRecords {
        tasks: Vec::new(),
        runs: Vec::new(),
        approvals: Vec::new(),
        attention: Vec::new(),
        withheld_approvals: Vec::new(),
    }
}

pub(crate) fn size(value: &impl serde::Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |v| v.len())
}

/// What `records` add to a list of records once encoded, separators included.
fn weight(records: &TaskRecords) -> usize {
    let each = |bytes: usize| bytes.saturating_add(1);
    (records.tasks.iter().map(|v| each(size(v))))
        .chain(records.runs.iter().map(|v| each(size(v))))
        .chain(records.approvals.iter().map(|v| each(size(v))))
        .chain(records.attention.iter().map(|v| each(size(v))))
        .fold(named(&records.withheld_approvals), usize::saturating_add)
}

/// What naming `ids` as withheld adds to a reply once encoded.
fn named(ids: &[ApprovalId]) -> usize {
    if ids.is_empty() {
        return 0;
    }
    (ids.iter().map(|id| size(id).saturating_add(1))).fold(WITHHELD_KEY, usize::saturating_add)
}

/// The `pending` approvals that fit within `budget` beside `used`, in order,
/// and the ids of the rest, which the phone reads one at a time with
/// `Query::Approval`. Every id is counted before any approval, so naming
/// those left out never takes the reply past `budget`. One too large to fit
/// does not keep a smaller one after it out.
fn within(
    pending: Vec<Approval>,
    used: &mut usize,
    budget: usize,
) -> (Vec<Approval>, Vec<ApprovalId>) {
    let ids: Vec<ApprovalId> = pending.iter().map(|a| a.approval_id).collect();
    *used = used.saturating_add(named(&ids));
    let mut withheld = Vec::new();
    let carried = (pending.into_iter())
        .filter(|approval| {
            let bytes = size(approval).saturating_add(1);
            let fits = used.saturating_add(bytes) <= budget;
            if fits {
                *used = used.saturating_add(bytes);
            } else {
                withheld.push(approval.approval_id);
            }
            fits
        })
        .collect();
    (carried, withheld)
}

fn append(into: &mut TaskRecords, from: TaskRecords) {
    into.tasks.extend(from.tasks);
    into.runs.extend(from.runs);
    into.approvals.extend(from.approvals);
    into.attention.extend(from.attention);
    into.withheld_approvals.extend(from.withheld_approvals);
}

/// `snapshot`'s tasks, runs, approvals and attention for a caller with `g`:
/// everything live, then the rest of each task newest first while the
/// snapshot stays within its budget. A live task's earlier runs are part of
/// its rest, so however many tasks are live their histories share the one
/// budget. For a caller that `reads_withheld`, pending approvals that would
/// take it past the budget are named in `withheld_approvals` instead; any
/// other caller gets all of them, or `IncompatibleVersion` when they do not
/// fit one frame.
pub(crate) fn fill(
    r: &store::Read<'_>,
    g: &Grants,
    snapshot: &mut Snapshot,
    reads_withheld: bool,
) -> store::Result<()> {
    if !g.tasks && !g.approvals {
        return Ok(());
    }
    let live = Records::load_live(r)?;
    let live_ids: HashSet<TaskId> = live.tasks.iter().map(|t| t.task_id).collect();
    let mut carried = empty();
    for task in &live.tasks {
        append(&mut carried, live.split_live(task, g).0);
    }
    if g.tasks {
        carried.attention.extend(live.unattached);
    }
    // Every approval carried so far is pending.
    let pending = std::mem::take(&mut carried.approvals);
    let mut used = size(snapshot).saturating_add(weight(&carried));
    if reads_withheld {
        (carried.approvals, snapshot.withheld_approvals) =
            within(pending, &mut used, SNAPSHOT_BYTES);
    } else {
        // An earlier phone knows nothing of withheld approvals, and would never
        // learn of one left out of its snapshot.
        used = (pending.iter())
            .map(|approval| size(approval).saturating_add(1))
            .fold(used, usize::saturating_add);
        if used > WHOLE_SNAPSHOT_BYTES {
            return Err(ProtocolError::new(
                ErrorCode::IncompatibleVersion,
                "Pending approvals are too large for this version of the app to load. Update the app to see them.",
            )
            .into());
        }
        carried.approvals = pending;
    }
    let mut before = None;
    while let Some(task) = r.history_snapshot_task(before)? {
        let from = cursor(&task);
        before = Some(from);
        let records = Records::for_tasks(r, vec![task], false, false)?;
        let task = &records.tasks[0];
        let (rest, bytes) = if live_ids.contains(&task.task_id) {
            let rest = records.split_live(task, g).1;
            let bytes = weight(&rest);
            (rest, bytes)
        } else {
            records.whole(task, g)
        };
        if used.saturating_add(bytes) > SNAPSHOT_BYTES {
            if g.tasks {
                snapshot.older_tasks = Some(OlderTasks {
                    from,
                    projects: r.history_older_projects(from)?,
                });
            }
            break;
        }
        used += bytes;
        append(&mut carried, rest);
    }
    snapshot.tasks = carried.tasks;
    snapshot.runs = carried.runs;
    snapshot.approvals = carried.approvals;
    snapshot.attention = carried.attention;
    Ok(())
}

/// What a snapshot for a caller with `g` carries besides tasks and what
/// belongs to them: the host, projects, control targets and library roots.
pub(crate) fn always(r: &store::Read<'_>, g: &Grants) -> store::Result<Snapshot> {
    Ok(Snapshot {
        journal_head: r.head()?,
        host: r.require("meta", "host")?,
        projects: if g.tasks {
            r.list("project")?
        } else {
            Vec::new()
        },
        tasks: Vec::new(),
        runs: Vec::new(),
        approvals: Vec::new(),
        attention: Vec::new(),
        targets: if g.view {
            r.get("meta", "targets")?.unwrap_or_default()
        } else {
            Vec::new()
        },
        control: if g.view {
            r.list("control")?
        } else {
            Vec::new()
        },
        library_roots: if g.library_read {
            r.get("meta", "library_roots")?.unwrap_or_default()
        } else {
            Vec::new()
        },
        older_tasks: None,
        withheld_approvals: Vec::new(),
    })
}

/// Refuses what a person just asked for when it leaves live tasks or any
/// Next up too large for a phone to load. Every live task travels in a
/// snapshot, and a queue in one reply, so neither can wait for a page.
pub(crate) fn admit(r: &store::Read<'_>) -> store::Result<()> {
    let live = live_weight(r)?;
    if live > LIVE_BYTES {
        return Err(ProtocolError::new(
            ErrorCode::LimitExceeded,
            "Unfinished tasks would be too large for the app to load. Finish or remove some tasks first, or shorten what you are adding.",
        )
        .into());
    }
    fits(r, live)?;
    if (r.list::<QueueView>("queue")?.iter()).any(|q| size(q) > QUEUE_BYTES) {
        return Err(ProtocolError::new(
            ErrorCode::LimitExceeded,
            "Next up can't hold that much. Shorten the request, or start or remove a waiting task first.",
        )
        .into());
    }
    Ok(())
}

/// Refuses a change to projects that leaves them, with what is live, too
/// large for a phone to load. Projects travel whole in every snapshot.
pub(crate) fn admit_projects(r: &store::Read<'_>) -> store::Result<()> {
    fits(r, live_weight(r)?)
}

/// Whether everything a snapshot must carry, `live` tasks among it, stays
/// within its budget, which leaves the rest of a frame for what the host
/// adds unasked: pending approvals, and what running tasks report.
fn fits(r: &store::Read<'_>, live: usize) -> store::Result<()> {
    if size(&always(r, &Grants::owner())?).saturating_add(live) > SNAPSHOT_BYTES {
        return Err(ProtocolError::new(
            ErrorCode::LimitExceeded,
            "Projects and unfinished tasks together would be too large for the app to load. Remove a project you no longer use, or finish or remove some tasks first.",
        )
        .into());
    }
    Ok(())
}

/// What everything live takes up in a snapshot, pending approvals aside,
/// with room for each Next up to ask for a go-ahead. Next up asks about its
/// front task once nothing else holds it back, and a rename, a resume or a
/// go-ahead that lapses can make it ask with nothing added, so the room is
/// kept for its heaviest waiting task whether or not it has asked yet. A
/// dismissed notice about a request still waiting is raised again, as a copy
/// of the latest, when a decision on it cannot be delivered, so while none is
/// open the latest keeps its room until the request settles.
pub(crate) fn live_weight(r: &store::Read<'_>) -> store::Result<usize> {
    let records = Records::load_live(r)?;
    let mut live = empty();
    for task in &records.tasks {
        let mut about = records.records(task, &Grants::owner());
        about.attention.retain(|a| a.resolved_at.is_none());
        append(&mut live, about);
    }
    live.attention.extend(records.unattached.iter().cloned());
    let mut waiting: HashSet<ApprovalId> =
        live.approvals.drain(..).map(|a| a.approval_id).collect();
    for a in &live.attention {
        if let Some(id) = a.approval_id {
            waiting.remove(&id);
        }
    }
    let mut back: HashMap<ApprovalId, &AttentionItem> = HashMap::new();
    for a in (records.attention.values().flatten()).filter(|a| a.resolved_at.is_some()) {
        if let Some(id) = a.approval_id.filter(|id| waiting.contains(id)) {
            let latest = back.entry(id).or_insert(a);
            if a.created_at > latest.created_at {
                *latest = a;
            }
        }
    }
    live.attention.extend(back.into_values().cloned());
    let tasks: HashMap<TaskId, &Task> = (records.tasks.iter()).map(|t| (t.task_id, t)).collect();
    // A request that will never be dated this late weighs as much as any.
    let latest = protocol::time::Timestamp(i64::MAX);
    let mut room = 0usize;
    for queue in r.list::<QueueView>("queue")? {
        let asked = r.get::<AttentionId>("queue_attention", queue.project_id)?;
        let asked = (live.attention.iter())
            .position(|a| Some(a.attention_id) == asked)
            .map(|index| live.attention.remove(index));
        let asking = (queue.items.iter())
            .filter(|item| item.is_pending())
            .filter_map(|item| tasks.get(&item.task_id))
            .map(|task| crate::runs::go_ahead(task, latest));
        room = (asked.into_iter().chain(asking))
            .map(|a| size(&a).saturating_add(1))
            .max()
            .map_or(room, |bytes| room.saturating_add(bytes));
    }
    Ok(weight(&live).saturating_add(room))
}

/// `snapshot`'s tasks, runs, approvals and attention, every one of them. A
/// chat connector reads the snapshot inside the companion, never through a
/// frame, and finds by id there the tasks and approvals its messages name.
pub(crate) fn fill_whole(
    r: &store::Read<'_>,
    g: &Grants,
    snapshot: &mut Snapshot,
) -> store::Result<()> {
    if g.tasks {
        snapshot.tasks = r.list("task")?;
        snapshot.runs = r.list("run")?;
        snapshot.attention = r.list("attention")?;
    }
    if g.approvals {
        snapshot.approvals = r.list("approval")?;
    }
    Ok(())
}

/// A page of `project`'s tasks from `from` on, newest first: at most `limit`,
/// and always at least one task, so that reading on reaches every one.
pub(crate) fn page(
    r: &store::Read<'_>,
    g: &Grants,
    project: ProjectId,
    from: TaskCursor,
    limit: u32,
) -> store::Result<QueryData> {
    let limit = limit.clamp(1, 1000) as usize;
    let tasks = r.history_tasks(project, from, limit + 1)?;
    let mut page = empty();
    let (mut used, mut next) = (size(&page), None);
    for (kept, task) in tasks.into_iter().enumerate() {
        if kept == limit {
            next = Some(cursor(&task));
            break;
        }
        let records = Records::for_tasks(r, vec![task], false, false)?;
        let task = &records.tasks[0];
        let (whole, bytes) = records.whole(task, g);
        if kept > 0 && used.saturating_add(bytes) > PAGE_BYTES {
            next = Some(cursor(task));
            break;
        }
        used = used.saturating_add(bytes);
        append(&mut page, whole);
    }
    Ok(QueryData::TaskHistory {
        records: page,
        next,
    })
}

/// One task with its records, as a page carries it.
pub(crate) fn task(r: &store::Read<'_>, g: &Grants, task_id: TaskId) -> store::Result<QueryData> {
    let task = r.require("task", task_id)?;
    let records = Records::for_tasks(r, vec![task], false, false)?;
    let task = &records.tasks[0];
    Ok(QueryData::Task {
        records: records.whole(task, g).0,
    })
}

/// One run whole, with the approvals and attention about it. Decided
/// approvals that would take it past a page are left out, and then pending
/// ones that do not fit are named instead.
pub(crate) fn run(r: &store::Read<'_>, g: &Grants, run_id: RunId) -> store::Result<QueryData> {
    let mut run: Run = r.require("run", run_id)?;
    let _ = crate::children::list_recent(&mut run);
    let mut about = empty();
    if g.approvals {
        about.approvals = (r.list::<Approval>("approval")?.into_iter())
            .filter(|a| a.run_id == run_id)
            .collect();
    }
    about.attention = (r.list::<AttentionItem>("attention")?.into_iter())
        .filter(|a| a.run_id == Some(run_id))
        .collect();
    if size(&run).saturating_add(weight(&about)) > PAGE_BYTES {
        about
            .approvals
            .retain(|a| a.state == ApprovalState::Pending);
    }
    if size(&run).saturating_add(weight(&about)) > PAGE_BYTES {
        let pending = std::mem::take(&mut about.approvals);
        let mut used = size(&run).saturating_add(weight(&about));
        (about.approvals, about.withheld_approvals) = within(pending, &mut used, PAGE_BYTES);
    }
    Ok(QueryData::Run {
        run: Box::new(run),
        approvals: about.approvals,
        attention: about.attention,
        withheld_approvals: about.withheld_approvals,
    })
}
