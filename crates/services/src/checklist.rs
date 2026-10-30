// SPDX-License-Identifier: Apache-2.0
//! A project's checklist (`protocol::checklist`): what its agents write for
//! the person to do themselves, and what the person ticks off, answers and
//! asks about. An item an agent waits on starts that agent again the moment
//! it is done, through a follow-up (`crate::followups`), whatever else is
//! still open; an answer or a question goes to the agent that wrote the item
//! the same way. An agent's own items, which it keeps itself, start it the
//! same way once nothing they come after is open, and the person's feedback
//! on one goes to it. Its files are kept as blobs, by their content, and
//! reach the person where they chose (`FileDelivery`).

use crate::{
    CompanionHub, error,
    followups::{FOLLOWUP, Followup},
    internal,
};
use protocol::{
    CommandId, ErrorCode, Event, ProjectId, ProtocolError, RunId, TaskId,
    checklist::*,
    model::{Project, Shown, Task, TaskWake},
    time::Timestamp,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

/// Each project's checklist, by project.
pub(crate) const CHECKLIST: &str = "checklist";
/// Where the person's checklist files go, in `meta`.
const DELIVERY: &str = "file_delivery";
/// How long a task shows as waiting on its checklist.
const WAITS_FOR: i64 = 30 * 24 * 60 * 60 * 1000;
/// How a wait on the checklist ends its reason, so it is told from others.
const ON_THE_CHECKLIST: &str = "on the checklist";
/// Tasks Ophio started to answer the person's question about a checklist
/// whose agent was put away. Their turns read the checklist and answer on
/// it, and do nothing else. Kept by the command that makes one until the
/// task is made, so its first turn has them too.
const ANSWERING: &str = "checklist_answering";
/// What a question tells its agent to do when it can change the checklist,
/// and what an agent answering for another does instead.
const FIX_ITEM: &str = " If a step was unclear, wrong or missing something, rewrite the item with checklist_add too. Then go back to what you were doing.";
const FIX_LIST: &str = " If the checklist was unclear, wrong or missing something, fix it with checklist_add too. Then go back to what you were doing.";
const SAY_SO: &str = " If something on it was unclear, wrong or missing, say so in your answer: you can't change the checklist here.";

/// What an agent adds to its project's checklist in one call, or
/// `ophio checklist put` does: what `checklist_add` takes.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Additions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default)]
    pub items: Vec<ItemDraft>,
}

/// What a put wrote: the titles, how many of them an agent waits on,
/// whether the checklist is new, whether any is an agent's own work still
/// to do, and what else the reply says.
struct Put {
    titles: Vec<String>,
    waiting: u32,
    made: bool,
    own: bool,
    said: Vec<String>,
}

impl CompanionHub {
    /// Every project's checklist at a glance, the latest changed first.
    pub(crate) fn checklists_view(&self) -> Result<Vec<ChecklistSummary>, ProtocolError> {
        self.read(|r| {
            let mut all: Vec<ChecklistSummary> = (r.list::<Checklist>(CHECKLIST)?.iter())
                .map(Checklist::summary)
                .collect();
            all.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
            Ok(all)
        })
    }

    pub(crate) fn checklist_view(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<Box<Checklist>>, ProtocolError> {
        self.read(|r| Ok(r.get::<Checklist>(CHECKLIST, project_id)?.map(Box::new)))
    }

    /// The project's checklist as its agent reads it.
    pub(crate) fn checklist_read(
        &self,
        run_id: RunId,
        task: &Task,
    ) -> Result<String, ProtocolError> {
        if self.run_stopped(run_id)? {
            return Err(error(ErrorCode::Expired, "Run is no longer active"));
        }
        let list = self.read(|r| r.get::<Checklist>(CHECKLIST, task.project_id))?;
        let list = match list {
            None => "This project has no checklist yet. Make one with checklist_add when the person has something to do themselves.\n".into(),
            Some(list) => render(&list, Some(task.task_id)),
        };
        Ok(format!(
            "{list}\n---\n# How to use the checklist\n\n{}\n",
            guide()
        ))
    }

    /// Adds `add`'s items to the project's checklist, or rewrites the ones
    /// with the same ids, and shows the person what changed.
    pub(crate) async fn checklist_add(
        &self,
        run_id: RunId,
        task: &Task,
        add: Additions,
        hide: impl Fn(&mut String),
    ) -> Result<String, ProtocolError> {
        let by = Some((run_id, task.task_id));
        let Put {
            titles,
            waiting,
            made,
            own,
            said,
        } = self.put(task.project_id, by, add, hide).await?;
        let mut reply = format!(
            "{} {} on the project's checklist. The person sees it under New in Chat and in Tasks, and a card for it in this conversation.",
            if made {
                "Made the checklist with"
            } else {
                "Put"
            },
            what(&titles)
        );
        if waiting > 0 {
            reply.push_str(" You wait on what you marked waiting: if there is nothing else you can do meanwhile, end your turn now with a short summary. Don't wait, sleep or poll. Ophio starts your next turn the moment the person finishes one, with what they answered.");
        } else if !own {
            reply.push_str(" Carry on with your work; nothing waits on it.");
        }
        if own {
            reply.push_str(" Your own items wait on what they come after: Ophio starts your next turn when the person has finished it. Once you've done one, mark it done with done: true.");
        }
        for line in said {
            reply.push(' ');
            reply.push_str(&line);
        }
        Ok(reply)
    }

    /// What `ophio checklist` shows, puts on and takes off a project's
    /// checklist, for the person at this computer or an agent outside
    /// Ophio. No agent of Ophio's waits on what it puts, and its files come
    /// from the project's folder.
    pub async fn owner_checklist(
        &self,
        project_id: ProjectId,
        put: Option<Additions>,
        remove: Vec<String>,
    ) -> Result<String, ProtocolError> {
        let mut said = Vec::new();
        if let Some(add) = put {
            if let Some(d) = add.items.iter().find(|d| d.waiting.is_some()) {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    format!(
                        "Leave waiting out of “{}”: no agent in Ophio waits on it. Tell the person yourself what to come back with.",
                        d.id
                    ),
                ));
            }
            let put = self.put(project_id, None, add, |_| {}).await?;
            let mut line = format!(
                "{} {}.",
                if put.made {
                    "Made the checklist with"
                } else {
                    "Put"
                },
                what(&put.titles)
            );
            for more in put.said {
                line.push(' ');
                line.push_str(&more);
            }
            said.push(line);
        }
        if !remove.is_empty() {
            said.push(self.take_off(
                project_id,
                None,
                &remove,
                "`ophio checklist show` lists the ids.",
            )?);
        }
        if !said.is_empty() {
            return Ok(said.join("\n") + "\n");
        }
        let list = self.read(|r| r.get::<Checklist>(CHECKLIST, project_id))?;
        Ok(match list {
            None => "This project has no checklist.\n".into(),
            Some(list) => render(&list, None),
        })
    }

    /// Writes `add` into `project_id`'s checklist, by `by`'s run and task
    /// or, with none, by the person.
    async fn put(
        &self,
        project_id: ProjectId,
        by: Option<(RunId, TaskId)>,
        mut add: Additions,
        hide: impl Fn(&mut String),
    ) -> Result<Put, ProtocolError> {
        let stopped = || -> Result<bool, ProtocolError> {
            Ok(match by {
                Some((run_id, _)) => self.run_stopped(run_id)?,
                None => false,
            })
        };
        if stopped()? {
            return Err(error(ErrorCode::Expired, "Run is no longer active"));
        }
        clean(&mut add, &hide)?;
        let project: Project = self.read(|r| r.require("project", project_id))?;
        // The person's files come from the project alone.
        let scratch = match by {
            Some((_, task_id)) => self.scratch_dir(task_id),
            None => PathBuf::from(&project.host_path),
        };
        let logo = add.theme.as_ref().and_then(|t| t.logo.clone());
        let paths: Vec<String> = (add.items.iter())
            .flat_map(|i| i.steps.iter().filter_map(|s| s.file.clone()))
            .chain(logo.clone())
            .collect();
        let mut files: HashMap<String, (ChecklistFile, Vec<u8>)> = HashMap::new();
        for path in paths {
            if files.contains_key(&path) {
                continue;
            }
            let read = read_file(&project, &scratch, &path, &hide).await?;
            files.insert(path, read);
        }
        if let Some(logo) = &logo
            && !files[logo].0.media_type.starts_with("image/")
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Give the logo as a PNG, JPEG, WebP or GIF picture",
            ));
        }
        // Held until the checklist names its files, so collection can't take
        // them in between.
        let _publishing = self.inner.blobs.read().await;
        for (file, bytes) in files.values() {
            self.put_blob(bytes, &file.sha256).await?;
        }
        let files: HashMap<String, ChecklistFile> =
            files.into_iter().map(|(p, (f, _))| (p, f)).collect();
        let now = self.inner.clock.now();
        self.write(|tx| {
            // A Stop that commits first wins.
            if let Some((run_id, _)) = by
                && crate::runs::stopped(&tx.read(), run_id)?
            {
                return Err(error(ErrorCode::Expired, "Run is no longer active").into());
            }
            let old = tx.read().get::<Checklist>(CHECKLIST, project_id)?;
            let made = old.is_none();
            let mut list = old.unwrap_or_else(|| Checklist {
                project_id: project.project_id,
                title: format!("{} checklist", project.name),
                summary: None,
                theme: None,
                sections: Vec::new(),
                notes: Vec::new(),
                task_id: None,
                created_at: now,
                updated_at: now,
                revision: 0,
            });
            let task_id = by.map(|(_, t)| t);
            // A rewrite moves an item off its old writer's wait.
            let writers: HashSet<TaskId> = (add.items.iter())
                .filter_map(|d| list.item(&d.id).and_then(|i| i.task_id))
                .filter(|w| Some(*w) != task_id)
                .collect();
            let before = list.clone();
            let add = Additions {
                items: owned(&list, &add.items)?,
                ..add.clone()
            };
            let titles = merge(&mut list, task_id, &add, &files, now);
            check(&list, &add.items)?;
            waits_on_the_person(&before, &list, &add.items)?;
            list.sort();
            let mut said = owners_changed(&before, &list, &add.items);
            said.extend(settle_ready(tx, &before, &list, task_id, None, now)?);
            let own = (add.items.iter()).any(|d| {
                list.item(&d.id)
                    .is_some_and(|i| i.for_agent() && !i.is_done())
            });
            let waiting = (add.items.iter())
                .filter(|d| list.item(&d.id).is_some_and(|i| !i.is_done()) && d.waiting.is_some())
                .count() as u32;
            let title = list.title.clone();
            save(tx, list, now)?;
            for writer in writers {
                refresh_wait(tx, writer, now)?;
            }
            if let Some((run_id, task_id)) = by {
                refresh_wait(tx, task_id, now)?;
                crate::shown::append(
                    tx,
                    now,
                    run_id,
                    task_id,
                    Shown::Checklist {
                        title,
                        items: titles.clone(),
                        waiting,
                    },
                )?;
            }
            Ok(Put {
                titles,
                waiting,
                made,
                own,
                said,
            })
        })
    }

    /// Takes `items` off the project's checklist.
    pub(crate) fn checklist_remove(
        &self,
        run_id: RunId,
        task: &Task,
        items: Vec<String>,
    ) -> Result<String, ProtocolError> {
        if self.run_stopped(run_id)? {
            return Err(error(ErrorCode::Expired, "Run is no longer active"));
        }
        let by = Some((run_id, task.task_id));
        self.take_off(task.project_id, by, &items, "checklist_read lists the ids.")
    }

    /// Takes `items` off `project_id`'s checklist, by `by`'s run and task
    /// or, with none, by the person, and says so, with `hint` when some
    /// weren't on it.
    fn take_off(
        &self,
        project_id: ProjectId,
        by: Option<(RunId, TaskId)>,
        items: &[String],
        hint: &str,
    ) -> Result<String, ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            if let Some((run_id, _)) = by
                && crate::runs::stopped(&tx.read(), run_id)?
            {
                return Err(error(ErrorCode::Expired, "Run is no longer active").into());
            }
            let mut list = require(&tx.read(), project_id)?;
            let before = list.clone();
            let mut missing = Vec::new();
            let mut writers = HashSet::new();
            for id in items {
                let mut found = false;
                for section in &mut list.sections {
                    section.items.retain(|i| {
                        let it = i.item_id == *id;
                        if it {
                            writers.extend(i.task_id);
                        }
                        found |= it;
                        !it
                    });
                }
                if !found {
                    missing.push(id.as_str());
                }
            }
            list.sections.retain(|s| !s.items.is_empty());
            let gone = items.len() - missing.len();
            let emptied = list.sections.is_empty();
            let mut said = Vec::new();
            if emptied {
                put_away(tx, project_id)?;
            } else if gone > 0 {
                said = settle_ready(tx, &before, &list, by.map(|(_, t)| t), None, now)?;
                save(tx, list, now)?;
            }
            for writer in writers {
                refresh_wait(tx, writer, now)?;
            }
            let mut reply = match (gone, emptied) {
                (0, _) => "Nothing was taken off.".to_owned(),
                (_, true) => "Took them off; that was all of it, so the checklist is gone.".into(),
                (1, false) => "Took 1 item off the checklist.".into(),
                (n, false) => format!("Took {n} items off the checklist."),
            };
            if !missing.is_empty() {
                reply.push_str(&format!(" Not on it: {}. {hint}", missing.join(", ")));
            }
            for line in said {
                reply.push(' ');
                reply.push_str(&line);
            }
            Ok(reply)
        })
    }

    /// The agent's answer to the person, under `item` or the whole checklist.
    pub(crate) fn checklist_reply(
        &self,
        run_id: RunId,
        task: &Task,
        item: Option<String>,
        mut text: String,
        hide: impl Fn(&mut String),
    ) -> Result<String, ProtocolError> {
        if self.run_stopped(run_id)? {
            return Err(error(ErrorCode::Expired, "Run is no longer active"));
        }
        hide(&mut text);
        let text = words(&text, MAX_TEXT, "the answer")?;
        let now = self.inner.clock.now();
        self.write(|tx| {
            if crate::runs::stopped(&tx.read(), run_id)? {
                return Err(error(ErrorCode::Expired, "Run is no longer active").into());
            }
            let mut list = require(&tx.read(), task.project_id)?;
            let note = ChecklistNote {
                note_id: protocol::CommandId::new().to_string(),
                from: NoteFrom::Agent,
                text,
                at: now,
            };
            let reply = match &item {
                Some(id) => {
                    let found = list.item_mut(id).ok_or_else(|| no_item(id))?;
                    noted(&mut found.notes, note);
                    format!(
                        "The person reads your answer under “{}” on the checklist.",
                        found.title
                    )
                }
                None => {
                    noted(&mut list.notes, note);
                    "The person reads your answer at the end of the checklist.".into()
                }
            };
            save(tx, list, now)?;
            Ok(reply)
        })
    }

    /// The person's choice of where checklist files go, and where they could.
    pub(crate) async fn file_delivery_view(&self) -> Result<FileDeliveryView, ProtocolError> {
        let settings = self.read(file_delivery)?;
        let (taildrop, drives) = tokio::join!(taildrop_targets(), rclone_remotes());
        Ok(FileDeliveryView {
            settings,
            taildrop,
            folders: synced_folders(self.inner.home.as_deref()),
            drives,
        })
    }

    /// Puts the checklist file `sha256` where the person chose. Where it went.
    pub(crate) async fn deliver_checklist_file(
        &self,
        project_id: ProjectId,
        sha256: &str,
    ) -> Result<String, ProtocolError> {
        let (file, project, settings) = self.read(|r| {
            let list = require(r, project_id)?;
            let file = (list.files().find(|f| f.sha256 == sha256).cloned()).ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    "That file is no longer on the checklist",
                )
            })?;
            let project: Project = r.require("project", project_id)?;
            Ok((file, project, file_delivery(r)?))
        })?;
        let blob = self.artifact_path(&file.sha256)?;
        let name = plain_name(&file.name);
        let folder = plain_name(&project.name);
        match settings.delivery {
            FileDelivery::Phone => Err(error(
                ErrorCode::InvalidRequest,
                "Checklist files come to the phone; download it there",
            )),
            FileDelivery::Folder { path } => {
                let dir = Path::new(&path).join(folder);
                let to = copy_into(&blob, &dir, &name, &file).await?;
                Ok(to.display().to_string())
            }
            FileDelivery::Taildrop { device } => {
                let mut command = quiet("tailscale");
                command.args(["file", "cp", "--name", &name]);
                command.arg(&blob).arg(format!("{device}:"));
                finish(command, Duration::from_secs(300), "Taildrop").await?;
                Ok(format!("{device}, by Taildrop"))
            }
            FileDelivery::Drive {
                remote,
                folder: within,
            } => {
                let within = within.trim_matches('/');
                let to = match within.is_empty() {
                    true => format!("{remote}:{folder}/{name}"),
                    false => format!("{remote}:{within}/{folder}/{name}"),
                };
                let mut command = quiet("rclone");
                command.arg("copyto").arg(&blob).arg(&to);
                finish(command, Duration::from_secs(900), "rclone").await?;
                Ok(to)
            }
        }
    }
}

/// The person's choice of where checklist files go.
pub(crate) fn file_delivery(r: &store::Read<'_>) -> store::Result<FileDeliverySettings> {
    Ok(r.get("meta", DELIVERY)?.unwrap_or_default())
}

/// Keeps the person's choice; a folder given from `~` is kept as the folder
/// under `home` it names.
pub(crate) fn set_file_delivery(
    tx: &mut store::Write<'_>,
    delivery: &FileDelivery,
    home: Option<&Path>,
    expected: Option<u64>,
) -> store::Result<()> {
    let current = file_delivery(&tx.read())?;
    domain::revision(expected, current.revision)?;
    let bad = |what: &str| -> store::Error { error(ErrorCode::InvalidRequest, what).into() };
    let mut delivery = delivery.clone();
    if let FileDelivery::Folder { path } = &mut delivery {
        let under = (path.strip_prefix("~/").or((path == "~").then_some("")))
            .zip(home)
            .and_then(|(under, home)| home.join(under).to_str().map(str::to_owned));
        if let Some(full) = under {
            *path = full.trim_end_matches('/').to_owned();
        }
    }
    match &delivery {
        FileDelivery::Phone => {}
        FileDelivery::Taildrop { device } => {
            if device.is_empty() || device.chars().count() > 120 || !plain(device) {
                return Err(bad("Give the device's name as Taildrop lists it"));
            }
        }
        FileDelivery::Folder { path } => {
            let path = Path::new(path);
            if !path.is_absolute() || !path.is_dir() {
                return Err(bad("Give a folder that is on this computer"));
            }
        }
        FileDelivery::Drive { remote, folder } => {
            let named =
                (remote.chars()).all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '));
            if remote.is_empty() || remote.chars().count() > 120 || !named {
                return Err(bad("Give the drive's name as rclone listremotes lists it"));
            }
            let parts_ok = (folder.split('/')).all(|p| p != ".." && p != ".");
            if folder.chars().count() > 500 || !plain(folder) || !parts_ok {
                return Err(bad(
                    "Give the folder in the drive as a plain path, such as Ophio",
                ));
            }
        }
    }
    let settings = FileDeliverySettings {
        delivery,
        revision: current.revision + 1,
    };
    tx.put("meta", DELIVERY, &settings)
}

/// The person ticked `item_id`, or one of its steps, done or not.
pub(crate) fn tick(
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    item_id: &str,
    step_id: Option<&str>,
    done: bool,
    now: Timestamp,
) -> store::Result<()> {
    let mut list = require(&tx.read(), project_id)?;
    let before = list.clone();
    let item = list.item_mut(item_id).ok_or_else(|| no_item(item_id))?;
    if item.for_agent() {
        return Err(error(
            ErrorCode::InvalidRequest,
            format!(
                "“{}” is the agent's to do. Send it feedback instead.",
                item.title
            ),
        )
        .into());
    }
    let was = item.is_done();
    match step_id {
        Some(step_id) => {
            let step = (item.steps.iter_mut().find(|s| s.step_id == step_id))
                .ok_or_else(|| error(ErrorCode::NotFound, "That step is no longer on the item"))?;
            step.done_at = done.then(|| step.done_at.unwrap_or(now));
            // The last step ticked finishes the item once its question is
            // answered too; a step unticked opens it again.
            item.done_at = match done {
                true if finished(item) => Some(item.done_at.unwrap_or(now)),
                true => item.done_at,
                false => None,
            };
        }
        None if done => {
            if item.ask.as_ref().is_some_and(|a| a.answer.is_none()) {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "Answer the item's question before ticking it off",
                )
                .into());
            }
            for step in &mut item.steps {
                step.done_at.get_or_insert(now);
            }
            item.done_at.get_or_insert(now);
        }
        None => item.done_at = None,
    }
    item.changed_at = now;
    let item = item.clone();
    match (was, item.is_done()) {
        (false, true) => finished_now(tx, &list, &item, now)?,
        // Not passed on yet: it isn't, now.
        (true, false) => tx.delete(FOLLOWUP, done_key(project_id, item_id))?,
        _ => {}
    }
    settle_ready(tx, &before, &list, None, Some(&item), now)?;
    save(tx, list, now)?;
    if let Some(writer) = item.task_id {
        refresh_wait(tx, writer, now)?;
    }
    Ok(())
}

/// The person answered what `item_id` asks.
pub(crate) fn answer(
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    item_id: &str,
    text: &str,
    now: Timestamp,
) -> store::Result<()> {
    let text = words(text, MAX_TEXT, "the answer")?;
    let mut list = require(&tx.read(), project_id)?;
    let before = list.clone();
    let item = list.item_mut(item_id).ok_or_else(|| no_item(item_id))?;
    let ask = (item.ask.as_mut())
        .ok_or_else(|| error(ErrorCode::InvalidRequest, "That item asks nothing"))?;
    ask.answer = Some(ChecklistAnswer { text, at: now });
    let was = item.is_done();
    if !was && finished(item) {
        item.done_at = Some(now);
    }
    item.changed_at = now;
    let item = item.clone();
    if !was && item.is_done() {
        finished_now(tx, &list, &item, now)?;
    }
    if !(item.is_done() && item.waiting.is_some()) {
        let ask = item.ask.as_ref().expect("answered above");
        let answer = &ask.answer.as_ref().expect("answered above").text;
        let rest = match (item.is_done(), item.waiting.is_some()) {
            (true, _) => "The item is done.",
            (false, true) => "The item isn't finished yet; Ophio tells you when it is.",
            (false, false) => "The item isn't finished yet.",
        };
        let text = format!(
            "The person answered the checklist item “{}” (`{}`), which asks “{}”:\n\n{answer}\n\n{rest} Use the answer now if it is what you needed; otherwise go on with what you were doing.",
            item.title, item.item_id, ask.question
        );
        tell(tx, &list, &item, answer_key(project_id, item_id), text, now)?;
    }
    settle_ready(tx, &before, &list, None, Some(&item), now)?;
    save(tx, list, now)?;
    if let Some(writer) = item.task_id {
        refresh_wait(tx, writer, now)?;
    }
    Ok(())
}

/// The person asks about `item_id`, or the whole checklist.
pub(crate) fn ask(
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    item_id: Option<&str>,
    text: &str,
    note_id: String,
    now: Timestamp,
) -> store::Result<()> {
    let text = words(text, MAX_TEXT, "the question")?;
    let mut list = require(&tx.read(), project_id)?;
    let note = ChecklistNote {
        note_id: note_id.clone(),
        from: NoteFrom::Person,
        text: text.clone(),
        at: now,
    };
    let key = format!("checklist-note:{project_id}:{note_id}");
    match item_id {
        Some(id) => {
            let item = list.item_mut(id).ok_or_else(|| no_item(id))?;
            noted(&mut item.notes, note);
            let item = item.clone();
            if item.for_agent() {
                // Only the agent that keeps it can act on it: with none, the
                // feedback stays under the item.
                if let Some(writer) = item.task_id {
                    let body = format!(
                        "The person sent feedback on your own item “{}” (`{}`) on the project's checklist:\n\n{text}\n\nAnswer them with checklist_reply, item \"{}\": they read it under the item. If it changes your plan, rewrite the item with checklist_add, or mark it done with done: true if it is. Then go back to what you were doing.",
                        item.title, item.item_id, item.item_id
                    );
                    let key = format!("{}{note_id}", feedback_prefix(project_id, &item.item_id));
                    Followup::checklist(key, writer, body, now).put(tx)?;
                }
                return save(tx, list, now);
            }
            let body = format!(
                "The person asks about “{}” (item `{}`) on the project's checklist:\n\n{text}\n\nAnswer them with checklist_reply, item \"{}\": they read it under the item.{FIX_ITEM}",
                item.title, item.item_id, item.item_id
            );
            tell(tx, &list, &item, key, body, now)?;
        }
        None => {
            noted(&mut list.notes, note);
            let body = format!(
                "The person asks about the project's checklist, “{}”:\n\n{text}\n\nAnswer them with checklist_reply and no item: they read it at the end of the checklist.{FIX_LIST}",
                list.title
            );
            if let Some(writer) = list.task_id {
                Followup::checklist(key, writer, body, now).put(tx)?;
            }
        }
    }
    save(tx, list, now)
}

/// The person put the project's checklist away.
pub(crate) fn remove(
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    now: Timestamp,
) -> store::Result<()> {
    let list = require(&tx.read(), project_id)?;
    put_away(tx, project_id)?;
    let writers: HashSet<TaskId> = list.items().filter_map(|i| i.task_id).collect();
    for writer in writers {
        refresh_wait(tx, writer, now)?;
    }
    Ok(())
}

/// Removes the project's checklist and what is still to pass on from it.
pub(crate) fn put_away(tx: &mut store::Write<'_>, project_id: ProjectId) -> store::Result<()> {
    if tx.read().get::<Checklist>(CHECKLIST, project_id)?.is_none() {
        return Ok(());
    }
    tx.delete(CHECKLIST, project_id)?;
    let ours = |key: &str| {
        [
            "checklist-done:",
            "checklist-answer:",
            "checklist-note:",
            "checklist-ready:",
            "checklist-feedback:",
        ]
        .iter()
        .any(|p| {
            key.strip_prefix(p)
                .is_some_and(|k| k.starts_with(&project_id.to_string()))
        })
    };
    for (key, _) in tx.read().entries::<Followup>(FOLLOWUP)? {
        if ours(&key) {
            tx.delete(FOLLOWUP, &key)?;
        }
    }
    tx.emit(Event::ChecklistChanged { project_id })?;
    Ok(())
}

/// The person stopped or archived `task_id`: nothing it wrote on the
/// checklist waits on it any more, and what they ticked or answered doesn't
/// start it again. Its own items are no task's to keep now, so nothing
/// starts it for them and feedback on them stays under them. Their
/// questions still get an answer.
pub(crate) fn let_go(
    tx: &mut store::Write<'_>,
    task_id: TaskId,
    now: Timestamp,
) -> store::Result<()> {
    for (key, f) in tx.read().entries::<Followup>(FOLLOWUP)? {
        if f.task_id == task_id
            && f.kind == crate::followups::FollowupKind::Checklist
            && !is_question(&key)
        {
            tx.delete(FOLLOWUP, &key)?;
        }
    }
    let Some(task) = tx.read().get::<Task>("task", task_id)? else {
        return Ok(());
    };
    if let Some(mut list) = tx.read().get::<Checklist>(CHECKLIST, task.project_id)? {
        let mut changed = false;
        for section in &mut list.sections {
            for item in &mut section.items {
                if item.task_id == Some(task_id) && !item.is_done() && item.waiting.is_some() {
                    item.waiting = None;
                    changed = true;
                }
                if item.task_id == Some(task_id) && item.for_agent() {
                    item.task_id = None;
                    changed = true;
                }
            }
        }
        if changed {
            save(tx, list, now)?;
        }
    }
    refresh_wait(tx, task_id, now)
}

/// Every blob a checklist names, so collection keeps it.
pub(crate) fn blobs(r: &store::Read<'_>) -> store::Result<Vec<String>> {
    Ok((r.list::<Checklist>(CHECKLIST)?.iter())
        .flat_map(|l| l.files().map(|f| f.sha256.clone()).collect::<Vec<_>>())
        .collect())
}

/// The file `sha256` on `project_id`'s checklist, if it is there.
pub(crate) fn file(
    r: &store::Read<'_>,
    project_id: ProjectId,
    sha256: &str,
) -> store::Result<Option<ChecklistFile>> {
    Ok(r.get::<Checklist>(CHECKLIST, project_id)?
        .and_then(|l| l.files().find(|f| f.sha256 == sha256).cloned()))
}

/// What `task_id`'s agent waits on in its project's checklist: the titles of
/// the open items it wrote marked waiting.
pub(crate) fn waited_on(
    r: &store::Read<'_>,
    project_id: ProjectId,
    task_id: TaskId,
) -> store::Result<Vec<String>> {
    Ok(r.get::<Checklist>(CHECKLIST, project_id)?
        .map(|l| {
            l.items()
                .filter(|i| !i.is_done() && i.waiting.is_some() && i.task_id == Some(task_id))
                .map(|i| i.title.clone())
                .collect()
        })
        .unwrap_or_default())
}

/// Shows `task_id` as waiting on the person while it waits on its
/// checklist, and no longer once it doesn't. A wait of its own, for a time
/// or another agent, is left: it wakes the agent anyway.
pub(crate) fn refresh_wait(
    tx: &mut store::Write<'_>,
    task_id: TaskId,
    now: Timestamp,
) -> store::Result<()> {
    let Some(mut task) = tx.read().get::<Task>("task", task_id)? else {
        return Ok(());
    };
    if task.state == protocol::model::TaskState::Archived {
        return Ok(());
    }
    let waits = waited_on(&tx.read(), task.project_id, task_id)?;
    let ours = |w: &TaskWake| w.answer && w.reason.ends_with(ON_THE_CHECKLIST);
    let wake = match (waits.as_slice(), &task.wake) {
        ([], Some(w)) if ours(w) => None,
        ([], _) => return Ok(()),
        (_, Some(w)) if !ours(w) => return Ok(()),
        ([one], _) => Some(wait_for(
            format!("you to finish “{one}” {ON_THE_CHECKLIST}"),
            now,
        )),
        (many, _) => Some(wait_for(
            format!("you to finish {} items {ON_THE_CHECKLIST}", many.len()),
            now,
        )),
    };
    if wake.as_ref().map(|w| &w.reason) == task.wake.as_ref().map(|w| &w.reason) {
        return Ok(());
    }
    task.wake = wake;
    task.updated_at = now;
    tx.put("task", task_id, &task)?;
    tx.emit(Event::TaskUpserted { task })?;
    Ok(())
}

fn wait_for(reason: String, now: Timestamp) -> TaskWake {
    TaskWake {
        at: Timestamp(now.0.saturating_add(WAITS_FOR)),
        reason,
        answer: true,
    }
}

/// Whether a follow-up carries the person's question about the checklist.
pub(crate) fn is_question(key: &str) -> bool {
    key.starts_with("checklist-note:")
}

/// A question as an agent answering for the one put away reads it: it
/// can't change the checklist, only say what is wrong with it.
pub(crate) fn for_another(question: &str) -> String {
    question.replace(FIX_ITEM, SAY_SO).replace(FIX_LIST, SAY_SO)
}

/// The task `command` makes answers the person on the checklist.
pub(crate) fn will_answer(tx: &mut store::Write<'_>, command: CommandId) -> store::Result<()> {
    tx.put(ANSWERING, format!("command:{command}"), &true)
}

/// `command` made `task_id`: if Ophio made it to answer on the checklist,
/// it does.
pub(crate) fn made(
    tx: &mut store::Write<'_>,
    command: CommandId,
    task_id: TaskId,
) -> store::Result<()> {
    let key = format!("command:{command}");
    if tx.read().get::<bool>(ANSWERING, &key)?.is_some() {
        tx.delete(ANSWERING, &key)?;
        tx.put(ANSWERING, task_id, &true)?;
    }
    Ok(())
}

/// Forgets `command`'s mark when it made nothing.
pub(crate) fn wont_answer(tx: &mut store::Write<'_>, command: CommandId) -> store::Result<()> {
    tx.delete(ANSWERING, format!("command:{command}"))
}

/// Whether `task_id` answers the person on the checklist for an agent put
/// away, and so has the checklist's read and reply tools alone.
pub(crate) fn answers(r: &store::Read<'_>, task_id: TaskId) -> store::Result<bool> {
    Ok(r.get::<bool>(ANSWERING, task_id)?.is_some())
}

fn done_key(project_id: ProjectId, item_id: &str) -> String {
    format!("checklist-done:{project_id}:{item_id}")
}

fn answer_key(project_id: ProjectId, item_id: &str) -> String {
    format!("checklist-answer:{project_id}:{item_id}")
}

/// The follow-up telling an agent its own item `item_id` can be done now.
fn ready_key(project_id: ProjectId, item_id: &str) -> String {
    format!("checklist-ready:{project_id}:{item_id}")
}

/// How the follow-ups carrying the person's feedback on agent's item
/// `item_id` begin: one per note.
fn feedback_prefix(project_id: ProjectId, item_id: &str) -> String {
    format!("checklist-feedback:{project_id}:{item_id}:")
}

/// Whether everything `item` takes is done: its steps, and its question
/// answered.
fn finished(item: &ChecklistItem) -> bool {
    item.steps.iter().all(|s| s.done_at.is_some())
        && item.ask.as_ref().is_none_or(|a| a.answer.is_some())
}

/// `item` was just finished: the agent that waits on it goes on.
fn finished_now(
    tx: &mut store::Write<'_>,
    list: &Checklist,
    item: &ChecklistItem,
    now: Timestamp,
) -> store::Result<()> {
    let Some(waiting) = &item.waiting else {
        return Ok(());
    };
    let answer = match &item.ask {
        Some(ChecklistAsk {
            question,
            answer: Some(a),
            ..
        }) => format!(" They answered “{question}”:\n\n{}\n", a.text),
        _ => String::new(),
    };
    let text = format!(
        "The person finished “{}” (item `{}`) on the project's checklist. You were waiting on it to {}.{answer}\nCarry on with that now. What else is open on the checklist can wait; Ophio tells you when what you wait on is done.",
        item.title,
        item.item_id,
        waiting.trim_end_matches('.')
    );
    tell(
        tx,
        list,
        item,
        done_key(list.project_id, &item.item_id),
        text,
        now,
    )
}

/// Whether `item` is an agent's own work to do now: open, kept by a task,
/// and with nothing it comes after still open.
fn to_do_now(list: &Checklist, item: &ChecklistItem) -> bool {
    item.for_agent() && !item.is_done() && item.task_id.is_some() && list.ready(item)
}

/// Tells each agent of its own items that `before` became `after` made
/// ready to do, and takes back what it was told of those that aren't now.
/// One follow-up per item, so ticking, unticking, finishing or taking off
/// any one changes only what it should. `finished` is the person's item
/// that did it, when one did. What `actor`'s agent did itself it isn't
/// sent: the lines for its reply instead. Feedback not yet passed on goes
/// with an item to the task that keeps it now.
fn settle_ready(
    tx: &mut store::Write<'_>,
    before: &Checklist,
    after: &Checklist,
    actor: Option<TaskId>,
    finished: Option<&ChecklistItem>,
    now: Timestamp,
) -> store::Result<Vec<String>> {
    let project_id = after.project_id;
    for was in before.items().filter(|i| i.for_agent()) {
        let is = after.item(&was.item_id);
        let kept = is.filter(|i| i.for_agent()).and_then(|i| i.task_id);
        let still = is.is_some_and(|i| to_do_now(after, i)) && kept == was.task_id;
        if !still {
            tx.delete(FOLLOWUP, ready_key(project_id, &was.item_id))?;
        }
        if kept != was.task_id {
            hand_on_feedback(tx, project_id, &was.item_id, kept)?;
        }
    }
    let mut said = Vec::new();
    for item in after.items().filter(|i| to_do_now(after, i)) {
        let was = (before.item(&item.item_id))
            .is_some_and(|i| to_do_now(before, i) && i.task_id == item.task_id);
        let Some(keeper) = item.task_id.filter(|_| !was) else {
            continue;
        };
        if Some(keeper) == actor {
            said.push(format!(
                "Nothing your item “{}” (`{}`) comes after is open now: do it now, then mark it done with done: true.",
                item.title, item.item_id
            ));
            continue;
        }
        let text = match finished.filter(|f| item.after.contains(&f.item_id)) {
            Some(f) => format!(
                "The person finished “{}” (item `{}`) on the project's checklist, which your item “{}” (`{}`) came after. Nothing it needs is open now.\nDo it now, then mark it done with checklist_add (`done: true`).",
                f.title, f.item_id, item.title, item.item_id
            ),
            None => format!(
                "Nothing your item “{}” (`{}`) on the project's checklist comes after is open now.\nDo it now, then mark it done with checklist_add (`done: true`).",
                item.title, item.item_id
            ),
        };
        Followup::checklist(ready_key(project_id, &item.item_id), keeper, text, now).put(tx)?;
    }
    Ok(said)
}

/// Feedback on agent's item `item_id` not yet passed on goes to `keeper`,
/// the task that keeps it now, or stays under the item with none.
fn hand_on_feedback(
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    item_id: &str,
    keeper: Option<TaskId>,
) -> store::Result<()> {
    let prefix = feedback_prefix(project_id, item_id);
    for (key, mut f) in tx.read().entries::<Followup>(FOLLOWUP)? {
        if !key.starts_with(&prefix) {
            continue;
        }
        match keeper {
            Some(keeper) => {
                // A copy already on its way to the old keeper isn't current.
                f.task_id = keeper;
                f.command_id = CommandId::new();
                f.put(tx)?;
            }
            None => tx.delete(FOLLOWUP, &key)?,
        }
    }
    Ok(())
}

/// Passes `text` on to the agent that wrote `item`, or else the checklist.
fn tell(
    tx: &mut store::Write<'_>,
    list: &Checklist,
    item: &ChecklistItem,
    key: String,
    text: String,
    now: Timestamp,
) -> store::Result<()> {
    match item.task_id.or(list.task_id) {
        Some(task_id) => Followup::checklist(key, task_id, text, now).put(tx),
        None => Ok(()),
    }
}

fn require(r: &store::Read<'_>, project_id: ProjectId) -> store::Result<Checklist> {
    r.get::<Checklist>(CHECKLIST, project_id)?
        .ok_or_else(|| error(ErrorCode::NotFound, "This project has no checklist now").into())
}

fn no_item(id: &str) -> ProtocolError {
    error(
        ErrorCode::NotFound,
        format!("“{id}” is no longer on the checklist"),
    )
}

fn save(tx: &mut store::Write<'_>, mut list: Checklist, now: Timestamp) -> store::Result<()> {
    list.revision += 1;
    list.updated_at = now;
    let project_id = list.project_id;
    tx.put(CHECKLIST, project_id, &list)?;
    tx.emit(Event::ChecklistChanged { project_id })?;
    Ok(())
}

/// Keeps `note`, and only the newest notes.
fn noted(notes: &mut Vec<ChecklistNote>, note: ChecklistNote) {
    notes.push(note);
    let over = notes.len().saturating_sub(MAX_NOTES);
    notes.drain(..over);
}

/// `text` trimmed, refused when empty or longer than `max` characters.
fn words(text: &str, max: usize, what: &str) -> Result<String, ProtocolError> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > max {
        return Err(error(
            ErrorCode::InvalidRequest,
            format!("Give {what} in 1 to {max} characters"),
        ));
    }
    Ok(text.to_owned())
}

/// No control characters, and nothing that reorders what is shown.
fn plain(text: &str) -> bool {
    !(text.chars()).any(|c| c.is_control() || ports::library::direction_control(c))
}

/// Checks what an agent wrote and hides in it what the run hides. What the
/// person opens or copies is refused instead: hidden, it wouldn't work.
fn clean(add: &mut Additions, hide: &impl Fn(&mut String)) -> Result<(), ProtocolError> {
    let bad = |what: String| error(ErrorCode::InvalidRequest, what);
    let prose = |text: &mut String, max: usize, what: &str| -> Result<(), ProtocolError> {
        hide(text);
        *text = words(text, max, what)?;
        Ok(())
    };
    let exact = |text: &str, what: &str| -> Result<(), ProtocolError> {
        let mut seen = text.to_owned();
        hide(&mut seen);
        if seen != text {
            return Err(bad(format!(
                "{what} holds a key or token this run was given, which the person isn't shown; tell them where they can find it themselves"
            )));
        }
        Ok(())
    };
    if add.items.is_empty() && add.title.is_none() && add.summary.is_none() && add.theme.is_none() {
        return Err(bad("Give the items to put on the checklist".into()));
    }
    if let Some(title) = &mut add.title {
        prose(title, MAX_TITLE, "the checklist's title")?;
    }
    if let Some(summary) = &mut add.summary {
        hide(summary);
        *summary = summary.trim().to_owned();
        if summary.chars().count() > MAX_TEXT {
            return Err(bad(format!("Keep the summary to {MAX_TEXT} characters")));
        }
    }
    if let Some(section) = &mut add.section {
        prose(section, MAX_TITLE, "the section's title")?;
    }
    if let Some(theme) = &mut add.theme {
        if !colour(&theme.accent) || theme.surface.as_deref().is_some_and(|s| !colour(s)) {
            return Err(bad("Give the theme's colours as #rrggbb".into()));
        }
        if let Some(name) = &mut theme.name {
            prose(name, MAX_TITLE, "the theme's name")?;
        }
    }
    let mut ids = HashSet::new();
    for item in &mut add.items {
        if !slug(&item.id) {
            return Err(bad(format!(
                "Give “{}” an id of lowercase letters, digits and dashes, at most {MAX_ID} characters, such as grant-api-access",
                item.id
            )));
        }
        if !ids.insert(item.id.clone()) {
            return Err(bad(format!("“{}” is given twice", item.id)));
        }
        prose(&mut item.title, MAX_TITLE, "each item's title")?;
        if let Some(why) = &mut item.why {
            prose(why, MAX_TEXT, "why the item matters")?;
        }
        if let Some(waiting) = &mut item.waiting {
            prose(waiting, MAX_TEXT, "what you do once the item is done")?;
        }
        if let Some(due) = &item.due
            && !day(due)
        {
            return Err(bad(format!("Give “{}”'s due date as YYYY-MM-DD", item.id)));
        }
        if item.minutes.is_some_and(|m| m == 0 || m > 7 * 24 * 60) {
            return Err(bad("Give minutes as about how long it takes".into()));
        }
        for after in &item.after {
            if !slug(after) || *after == item.id {
                return Err(bad(format!(
                    "“{}” comes after “{after}”, which isn't another item's id",
                    item.id
                )));
            }
        }
        if item.steps.len() > MAX_STEPS {
            return Err(bad(format!(
                "Give “{}” at most {MAX_STEPS} steps; split it into items",
                item.id
            )));
        }
        let mut steps = HashSet::new();
        for (n, step) in item.steps.iter_mut().enumerate() {
            let id = step.id.get_or_insert_with(|| (n + 1).to_string());
            if !slug(id) || !steps.insert(id.clone()) {
                return Err(bad(format!(
                    "Give “{}”'s steps different ids of lowercase letters, digits and dashes",
                    item.id
                )));
            }
            prose(&mut step.text, MAX_TEXT, "each step")?;
            if let Some(url) = &mut step.url {
                *url = link(url)?;
                exact(url, "A link")?;
            }
            if let Some(copy) = &step.copy {
                if copy.is_empty() || copy.chars().count() > MAX_TEXT {
                    return Err(bad(format!(
                        "Give each value to copy in 1 to {MAX_TEXT} characters"
                    )));
                }
                exact(copy, "A value to copy")?;
            }
        }
        if let Some(ask) = &mut item.ask {
            prose(&mut ask.question, MAX_TEXT, "the question")?;
            if ask.options.len() > MAX_OPTIONS {
                return Err(bad(format!("Offer at most {MAX_OPTIONS} options")));
            }
            for option in &mut ask.options {
                prose(&mut option.label, MAX_TITLE, "each option")?;
                if let Some(detail) = &mut option.detail {
                    prose(detail, MAX_TEXT, "each option's detail")?;
                }
            }
        }
    }
    Ok(())
}

/// A link the person opens: `https:`, `mailto:` or `tel:`.
fn link(raw: &str) -> Result<String, ProtocolError> {
    let refused = || {
        error(
            ErrorCode::InvalidRequest,
            "Give each link as an https:, mailto: or tel: address",
        )
    };
    let raw = raw.trim();
    if raw.len() > crate::shown::MAX_ADDRESS || !plain(raw) {
        return Err(refused());
    }
    let url = url::Url::parse(raw).map_err(|_| refused())?;
    match url.scheme() {
        "https" if url.host().is_some() => {}
        "mailto" | "tel" => {}
        _ => return Err(refused()),
    }
    Ok(url.to_string())
}

fn slug(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && (id.chars()).all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn colour(text: &str) -> bool {
    text.len() == 7 && text.starts_with('#') && text[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn day(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && (b.iter().enumerate()).all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && matches!(text[5..7].parse::<u8>(), Ok(1..=12))
        && matches!(text[8..10].parse::<u8>(), Ok(1..=31))
}

/// A section's id from its title.
fn section_id(title: &str) -> String {
    let mut id = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    let id: String = id.trim_end_matches('-').chars().take(MAX_ID).collect();
    match id.is_empty() {
        true => "section".into(),
        false => id,
    }
}

/// Writes `add` into `list`: new items where they go, rewritten ones where
/// they were. Ticks, answers and notes carry over by id. The titles written.
fn merge(
    list: &mut Checklist,
    task_id: Option<TaskId>,
    add: &Additions,
    files: &HashMap<String, ChecklistFile>,
    now: Timestamp,
) -> Vec<String> {
    if let Some(title) = &add.title {
        list.title = title.clone();
    }
    if let Some(summary) = &add.summary {
        list.summary = (!summary.is_empty()).then(|| summary.clone());
    }
    if let Some(theme) = &add.theme {
        list.theme = Some(ChecklistTheme {
            name: theme.name.clone(),
            accent: theme.accent.to_ascii_lowercase(),
            surface: theme.surface.as_ref().map(|s| s.to_ascii_lowercase()),
            font: theme.font,
            logo: theme.logo.as_ref().map(|p| files[p].clone()),
        });
    }
    if task_id.is_some() {
        list.task_id = task_id;
    }
    let named = add.section.as_ref().map(|name| {
        let id = section_id(name);
        match (list.sections.iter())
            .position(|s| s.section_id == id || s.title.eq_ignore_ascii_case(name))
        {
            Some(at) => at,
            None => {
                let mut unique = id.clone();
                let mut n = 2;
                while list.sections.iter().any(|s| s.section_id == unique) {
                    unique = format!("{id}-{n}");
                    n += 1;
                }
                list.sections.push(ChecklistSection {
                    section_id: unique,
                    title: name.clone(),
                    items: Vec::new(),
                });
                list.sections.len() - 1
            }
        }
    });
    let mut titles = Vec::new();
    for draft in &add.items {
        let old = (list.sections.iter_mut().enumerate()).find_map(|(s, section)| {
            let at = section.items.iter().position(|i| i.item_id == draft.id)?;
            Some((s, at, section.items.remove(at)))
        });
        let item = build(draft, old.as_ref().map(|(_, _, i)| i), task_id, files, now);
        let (section, at) = match (named, &old) {
            (Some(s), Some((was, at, _))) if s == *was => (s, Some(*at)),
            (Some(s), _) => (s, None),
            (None, Some((was, at, _))) => (*was, Some(*at)),
            (None, None) => {
                if list.sections.is_empty() {
                    list.sections.push(ChecklistSection {
                        section_id: "to-do".into(),
                        title: "To do".into(),
                        items: Vec::new(),
                    });
                }
                (list.sections.len() - 1, None)
            }
        };
        let items = &mut list.sections[section].items;
        match at {
            Some(at) => items.insert(at.min(items.len()), item),
            None => items.push(item),
        }
        titles.push(draft.title.clone());
    }
    list.sections.retain(|s| !s.items.is_empty());
    titles
}

fn build(
    draft: &ItemDraft,
    old: Option<&ChecklistItem>,
    task_id: Option<TaskId>,
    files: &HashMap<String, ChecklistFile>,
    now: Timestamp,
) -> ChecklistItem {
    let kept = old.filter(|_| !draft.reopen);
    let steps = (draft.steps.iter())
        .map(|s| {
            let step_id = s.id.clone().unwrap_or_default();
            let done_at = (kept.and_then(|o| o.steps.iter().find(|x| x.step_id == step_id)))
                .and_then(|x| x.done_at);
            ChecklistStep {
                step_id,
                text: s.text.clone(),
                url: s.url.clone(),
                copy: s.copy.clone(),
                file: s.file.as_ref().map(|p| files[p].clone()),
                done_at,
            }
        })
        .collect();
    let ask = draft.ask.as_ref().map(|a| ChecklistAsk {
        question: a.question.clone(),
        options: a.options.clone(),
        answer: (kept.and_then(|o| o.ask.as_ref()))
            .filter(|o| o.question == a.question)
            .and_then(|o| o.answer.clone()),
    });
    let mut item = ChecklistItem {
        item_id: draft.id.clone(),
        title: draft.title.clone(),
        owner: draft.owner.unwrap_or_default(),
        why: draft.why.clone(),
        minutes: draft.minutes,
        priority: draft.priority,
        due: draft.due.clone(),
        after: draft.after.clone(),
        steps,
        ask,
        waiting: draft.waiting.clone(),
        task_id,
        notes: old.map(|o| o.notes.clone()).unwrap_or_default(),
        done_at: kept.and_then(|o| o.done_at),
        added_at: old.map_or(now, |o| o.added_at),
        changed_at: now,
    };
    // An agent marks its own item done, or open again, itself.
    match draft.done {
        Some(true) => {
            for step in &mut item.steps {
                step.done_at.get_or_insert(now);
            }
            item.done_at.get_or_insert(now);
        }
        Some(false) => {
            for step in &mut item.steps {
                step.done_at = None;
            }
            item.done_at = None;
        }
        None => {}
    }
    item
}

/// Refuses a checklist over its limits, or with items that come after what
/// isn't there or after each other.
fn check(list: &Checklist, drafts: &[ItemDraft]) -> Result<(), ProtocolError> {
    let bad = |what: String| error(ErrorCode::LimitExceeded, what);
    if list.sections.len() > MAX_SECTIONS {
        return Err(bad(format!(
            "A checklist has at most {MAX_SECTIONS} sections"
        )));
    }
    if list.items().count() > MAX_ITEMS {
        return Err(bad(format!(
            "A checklist has at most {MAX_ITEMS} items; take done or stale ones off with checklist_remove"
        )));
    }
    let files: HashSet<&str> = list.files().map(|f| f.sha256.as_str()).collect();
    if files.len() > MAX_FILES {
        return Err(bad(format!(
            "A checklist carries at most {MAX_FILES} files"
        )));
    }
    for draft in drafts {
        if let Some(missing) = draft.after.iter().find(|a| list.item(a).is_none()) {
            return Err(error(
                ErrorCode::InvalidRequest,
                format!(
                    "“{}” comes after “{missing}”, which isn't on the checklist",
                    draft.id
                ),
            ));
        }
    }
    // Each item comes after others only so far back: none waits on itself.
    let after: HashMap<&str, &[String]> = (list.items())
        .map(|i| (i.item_id.as_str(), i.after.as_slice()))
        .collect();
    for start in after.keys() {
        let mut seen = HashSet::new();
        let mut todo: Vec<&str> = after[start].iter().map(String::as_str).collect();
        while let Some(id) = todo.pop() {
            if id == *start {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    format!("“{start}” comes, through others, after itself"),
                ));
            }
            if seen.insert(id) {
                todo.extend(
                    after
                        .get(id)
                        .into_iter()
                        .flat_map(|a| a.iter().map(String::as_str)),
                );
            }
        }
    }
    Ok(())
}

/// `drafts` with each one's owner settled: as given, or the item's own on a
/// rewrite, or the person's for a new item. Refuses what doesn't fit its
/// owner.
fn owned(list: &Checklist, drafts: &[ItemDraft]) -> Result<Vec<ItemDraft>, ProtocolError> {
    let bad = |what: String| error(ErrorCode::InvalidRequest, what);
    let mut settled = Vec::new();
    for draft in drafts {
        let owner = (draft.owner)
            .or_else(|| list.item(&draft.id).map(|i| i.owner))
            .unwrap_or_default();
        match owner {
            ChecklistOwner::Person if draft.done.is_some() => {
                return Err(bad(format!(
                    "Leave done out of “{}”: only the person ticks off their own items.",
                    draft.id
                )));
            }
            ChecklistOwner::Person if draft.steps.is_empty() && draft.ask.is_none() => {
                return Err(bad(format!(
                    "Give “{}” its steps, or a question to answer",
                    draft.id
                )));
            }
            ChecklistOwner::Agent if draft.waiting.is_some() => {
                return Err(bad(format!(
                    "Leave waiting out of “{}”: it is your own work. Name what it waits on in after; Ophio starts you when that is done.",
                    draft.id
                )));
            }
            ChecklistOwner::Agent if draft.ask.is_some() => {
                return Err(bad(format!(
                    "Leave ask out of “{}”: it is your own work. Ask the person on an item of theirs.",
                    draft.id
                )));
            }
            _ => {}
        }
        settled.push(ItemDraft {
            owner: Some(owner),
            ..draft.clone()
        });
    }
    Ok(settled)
}

/// Refuses an agent's own item, new or made the agent's now, that waits on
/// nothing the person has to do: work nothing blocks the agent just does.
/// Rewritten, one still open keeps an item of the person's in after, done
/// or not, so an edit can't leave it waiting on nothing, unless what it
/// waited on was taken off already.
fn waits_on_the_person(
    before: &Checklist,
    list: &Checklist,
    drafts: &[ItemDraft],
) -> Result<(), ProtocolError> {
    let theirs = |l: &Checklist, after: &[String], open: bool| {
        (after.iter()).any(|a| {
            l.item(a)
                .is_some_and(|i| !i.for_agent() && !(open && i.is_done()))
        })
    };
    for draft in drafts {
        let Some(item) = list.item(&draft.id).filter(|i| i.for_agent()) else {
            continue;
        };
        let refused = match before.item(&draft.id).filter(|i| i.for_agent()) {
            None => !theirs(list, &item.after, true),
            Some(was) => {
                !item.is_done()
                    && !theirs(list, &item.after, false)
                    && theirs(before, &was.after, false)
            }
        };
        if refused {
            return Err(error(
                ErrorCode::InvalidRequest,
                match before.item(&draft.id).is_some_and(|i| i.for_agent()) {
                    false => format!(
                        "Put “{}” on the checklist only if it waits on something the person has to do: name their open item in after. Work that nothing blocks, just do.",
                        draft.id
                    ),
                    true => format!(
                        "Keep the person's item that “{}” comes after in after, done or not, or mark it done with done: true: it can't wait on nothing.",
                        draft.id
                    ),
                },
            ));
        }
    }
    Ok(())
}

/// What the reply says of items whose owner `drafts` changed.
fn owners_changed(before: &Checklist, list: &Checklist, drafts: &[ItemDraft]) -> Vec<String> {
    (drafts.iter())
        .filter_map(|d| {
            let was = before.item(&d.id)?;
            let is = list.item(&d.id)?;
            (was.owner != is.owner).then(|| match is.owner {
                ChecklistOwner::Agent => format!(
                    "“{}” is an agent's own work now: the person sees it but doesn't tick it off.",
                    is.title
                ),
                ChecklistOwner::Person => {
                    format!(
                        "“{}” is the person's to do now: they tick it off.",
                        is.title
                    )
                }
            })
        })
        .collect()
}

/// Reads the file an agent gave for a step or the logo, from the project or
/// its scratch folder.
async fn read_file(
    project: &Project,
    scratch: &Path,
    path: &str,
    hide: &impl Fn(&mut String),
) -> Result<(ChecklistFile, Vec<u8>), ProtocolError> {
    let (folder, rel) = crate::shown::shown_relative(project, scratch, path)?;
    let file = match folder {
        crate::shown::Folder::Project => crate::transfers::open_project_file(project, &rel)?,
        crate::shown::Folder::Scratch => crate::transfers::open_scratch_file(scratch, &rel)?,
    };
    let bytes = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(MAX_FILE + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    })
    .await
    .map_err(|_| internal("Reading the file stopped"))?
    .map_err(internal)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(error(
            ErrorCode::LimitExceeded,
            format!("{rel} is larger than 25 MiB; give the person a smaller file, or a link to it"),
        ));
    }
    use sha2::Digest;
    let sha256 = crate::transfers::hex(&sha2::Sha256::digest(&bytes));
    let mut name = rel.rsplit('/').next().unwrap_or(&rel).to_owned();
    hide(&mut name);
    let media_type = crate::shown::sniff(&bytes)
        .unwrap_or_else(|| media_type(&name))
        .to_owned();
    Ok((
        ChecklistFile {
            name,
            size: bytes.len() as u64,
            sha256,
            media_type,
        },
        bytes,
    ))
}

/// A file's type from its name, when its bytes don't say.
fn media_type(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("pdf") => "application/pdf",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("zip") => "application/zip",
        Some("apk") => "application/vnd.android.package-archive",
        Some("ics") => "text/calendar",
        Some("vcf") => "text/vcard",
        Some("svg") => "image/svg+xml",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// `titles` as a reply names them.
fn what(titles: &[String]) -> String {
    match titles {
        [one] => format!("“{one}”"),
        many => format!("{} items", many.len()),
    }
}

/// When and how agents use the checklist, as the built-in skill says it.
pub(crate) const SKILL: &str = include_str!("setup_skills/checklist.md");

/// What [`SKILL`] says to an agent with Ophio's tools: its body, without
/// how to do it from outside Ophio. It comes with the checklist, so the
/// tool list every turn carries needn't.
fn guide() -> &'static str {
    let body = SKILL.split_once("\n---\n").map_or(SKILL, |(_, b)| b);
    body.split_once("\n## Without")
        .map_or(body, |(b, _)| b)
        .trim()
}

/// The checklist as an agent reads it, with every id it needs to change it.
fn render(list: &Checklist, task_id: Option<TaskId>) -> String {
    use std::fmt::Write;
    let mut out = format!("# {}\n", list.title);
    if let Some(summary) = &list.summary {
        let _ = writeln!(out, "{summary}");
    }
    if let Some(theme) = &list.theme {
        let _ = writeln!(
            out,
            "Theme: {} accent {}{}",
            theme.name.as_deref().unwrap_or("unnamed"),
            theme.accent,
            theme
                .surface
                .as_ref()
                .map(|s| format!(", surface {s}"))
                .unwrap_or_default()
        );
    }
    let s = list.summary();
    let _ = writeln!(
        out,
        "{} open, {} of them blockers, {} done, {} waited on, {} questions unanswered. Next: {}",
        s.open,
        s.blockers,
        s.done,
        s.waiting,
        s.unanswered,
        s.next.as_deref().unwrap_or("nothing")
    );
    if s.for_agent > 0 {
        let _ = writeln!(out, "Agents' own items: {} open.", s.for_agent);
    }
    for section in &list.sections {
        let _ = writeln!(
            out,
            "\n## {} (section \"{}\")",
            section.title, section.section_id
        );
        for item in &section.items {
            let tick = if item.is_done() { "x" } else { " " };
            let _ = writeln!(out, "- [{tick}] {} — id \"{}\"", item.title, item.item_id);
            if item.for_agent() {
                let whose = match (task_id, item.task_id) {
                    (Some(you), Some(writer)) if you == writer => " (yours: you keep it)",
                    (Some(_), Some(_)) => " (another task keeps it)",
                    (Some(_), None) => " (no task keeps it now)",
                    (None, _) => "",
                };
                let _ = writeln!(out, "  for: an agent{whose}");
            }
            if let Some(why) = &item.why {
                let _ = writeln!(out, "  why: {why}");
            }
            if let Some(m) = item.minutes {
                let _ = writeln!(out, "  minutes: {m}");
            }
            if item.priority != ChecklistPriority::Medium {
                let _ = writeln!(out, "  priority: {}", item.priority.as_str());
            }
            if let Some(due) = &item.due {
                let _ = writeln!(out, "  suggested for: {due}");
            }
            if !item.after.is_empty() {
                let _ = writeln!(out, "  after: {}", item.after.join(", "));
            }
            if let Some(waiting) = &item.waiting {
                let whose = match (task_id, item.task_id) {
                    (Some(you), Some(writer)) if you == writer => "you",
                    (Some(_), _) => "another task",
                    (None, _) => "an agent",
                };
                let _ = writeln!(out, "  waiting ({whose}): {waiting}");
            }
            for step in &item.steps {
                let tick = if step.done_at.is_some() { "x" } else { " " };
                let _ = write!(out, "  {}. [{tick}] {}", step.step_id, step.text);
                if let Some(url) = &step.url {
                    let _ = write!(out, " | url: {url}");
                }
                if let Some(copy) = &step.copy {
                    let _ = write!(out, " | copy: {copy}");
                }
                if let Some(file) = &step.file {
                    let _ = write!(out, " | file: {} ({} bytes)", file.name, file.size);
                }
                out.push('\n');
            }
            if let Some(ask) = &item.ask {
                let _ = write!(out, "  asks: {}", ask.question);
                if !ask.options.is_empty() {
                    let labels: Vec<&str> = ask.options.iter().map(|o| o.label.as_str()).collect();
                    let _ = write!(out, " (options: {})", labels.join(" | "));
                }
                match &ask.answer {
                    Some(a) => {
                        let _ = writeln!(out, "\n  answered: {}", a.text);
                    }
                    None => out.push_str("\n  not answered yet\n"),
                }
            }
            notes(&mut out, &item.notes, "  ", item.for_agent());
        }
    }
    if !list.notes.is_empty() {
        out.push_str("\n## About the whole checklist\n");
        notes(&mut out, &list.notes, "", false);
    }
    out
}

/// `notes` as an agent reads them; on an agent's own item, what the person
/// says is feedback.
fn notes(out: &mut String, notes: &[ChecklistNote], indent: &str, feedback: bool) {
    use std::fmt::Write;
    for note in notes {
        let from = match (note.from, feedback) {
            (NoteFrom::Person, false) => "the person asks",
            (NoteFrom::Person, true) => "the person's feedback",
            (NoteFrom::Agent, _) => "an agent answered",
        };
        let _ = writeln!(out, "{indent}{from}: {}", note.text);
    }
    if notes.last().is_some_and(|n| n.from == NoteFrom::Person) {
        let _ = writeln!(out, "{indent}(unanswered: reply with checklist_reply)");
    }
}

/// `name` as a file or folder name anywhere: no separators, no control
/// characters, not hidden.
fn plain_name(name: &str) -> String {
    let cleaned: String = (name.chars())
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() || ports::library::direction_control(c) => '_',
            c => c,
        })
        .take(120)
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_owned();
    match cleaned.is_empty() {
        true => "file".into(),
        false => cleaned,
    }
}

/// Copies `blob` into `dir` as `name`, or as `name (2)` and on when another
/// file has that name. The same file already there is left as it is.
async fn copy_into(
    blob: &Path,
    dir: &Path,
    name: &str,
    file: &ChecklistFile,
) -> Result<PathBuf, ProtocolError> {
    let unavailable = |e: std::io::Error| {
        error(
            ErrorCode::TemporarilyUnavailable,
            format!("Couldn't put the file in {}: {e}", dir.display()),
        )
    };
    tokio::fs::create_dir_all(dir).await.map_err(unavailable)?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    for n in 1..100 {
        let to = match n {
            1 => dir.join(name),
            n => dir.join(format!("{stem} ({n}){ext}")),
        };
        match tokio::fs::metadata(&to).await {
            Ok(meta) => {
                if meta.len() == file.size && same(&to, &file.sha256).await {
                    return Ok(to);
                }
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(unavailable(e)),
        }
        // Copied under a hidden name first, so a drive app syncs it whole.
        let partial = dir.join(format!(".{}.ophio-partial", protocol::CommandId::new()));
        if let Err(e) = tokio::fs::copy(blob, &partial).await {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(unavailable(e));
        }
        tokio::fs::rename(&partial, &to)
            .await
            .map_err(unavailable)?;
        return Ok(to);
    }
    Err(error(
        ErrorCode::Conflict,
        format!("{} already holds 99 files named {name}", dir.display()),
    ))
}

async fn same(path: &Path, sha256: &str) -> bool {
    use sha2::Digest;
    tokio::fs::read(path)
        .await
        .is_ok_and(|bytes| crate::transfers::hex(&sha2::Sha256::digest(&bytes)) == sha256)
}

/// A program run without a window or input, stopped with Ophio.
fn quiet(program: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: Øphio may run without a console.
    command
}

/// Runs `command` to the end, within `wait`; what it printed when it fails.
async fn finish(
    mut command: tokio::process::Command,
    wait: Duration,
    name: &str,
) -> Result<String, ProtocolError> {
    let output = match tokio::time::timeout(wait, command.output()).await {
        Err(_) => {
            return Err(error(
                ErrorCode::TemporarilyUnavailable,
                format!("{name} took too long"),
            ));
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(error(
                ErrorCode::Unsupported,
                format!("{name} isn't installed on this computer"),
            ));
        }
        Ok(Err(e)) => return Err(internal(e)),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let said: String = (said.lines())
            .filter(|l| !l.trim().is_empty() && !l.starts_with("Warning: client version"))
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(300)
            .collect();
        return Err(error(
            ErrorCode::TemporarilyUnavailable,
            format!("{name} couldn't send it: {}", said.trim()),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Devices Taildrop can send to, or None without Tailscale.
async fn taildrop_targets() -> Option<Vec<String>> {
    let mut command = quiet("tailscale");
    command.args(["file", "cp", "--targets"]);
    let out = finish(command, Duration::from_secs(5), "Taildrop")
        .await
        .ok()?;
    Some(taildrop_names(&out))
}

fn taildrop_names(out: &str) -> Vec<String> {
    (out.lines())
        .filter_map(|line| line.split('\t').nth(1))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && plain(name))
        .collect()
}

/// Drives set up in rclone, or None without rclone.
async fn rclone_remotes() -> Option<Vec<String>> {
    let mut command = quiet("rclone");
    command.arg("listremotes");
    let out = finish(command, Duration::from_secs(5), "rclone")
        .await
        .ok()?;
    Some(
        (out.lines())
            .map(|l| l.trim().trim_end_matches(':').to_owned())
            .filter(|l| !l.is_empty() && plain(l))
            .collect(),
    )
}

/// Folders on the computer a drive app keeps in sync, and the downloads
/// folder, that are there.
fn synced_folders(home: Option<&Path>) -> Vec<DeliveryFolder> {
    let Some(home) = home else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut add = |label: String, path: PathBuf| {
        if path.is_dir()
            && !found
                .iter()
                .any(|f: &DeliveryFolder| Path::new(&f.path) == path)
        {
            found.push(DeliveryFolder {
                label,
                path: path.display().to_string(),
            });
        }
    };
    // macOS keeps every drive app's folder here.
    if let Ok(entries) = std::fs::read_dir(home.join("Library/CloudStorage")) {
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let label = match name.split_once('-') {
                Some(("GoogleDrive", _)) => "Google Drive".to_owned(),
                Some(("OneDrive", rest)) => format!("OneDrive ({rest})"),
                _ => name.clone(),
            };
            add(label, path);
        }
    }
    add(
        "iCloud Drive".into(),
        home.join("Library/Mobile Documents/com~apple~CloudDocs"),
    );
    for (label, dir) in [
        ("Google Drive", "Google Drive"),
        ("Google Drive", "GoogleDrive"),
        ("Google Drive", "My Drive"),
        ("Dropbox", "Dropbox"),
        ("OneDrive", "OneDrive"),
        ("Nextcloud", "Nextcloud"),
        ("Syncthing", "Sync"),
        ("Downloads", "Downloads"),
    ] {
        add(label.into(), home.join(dir));
    }
    // Windows mounts Google Drive as a drive of its own.
    #[cfg(windows)]
    add("Google Drive".into(), PathBuf::from(r"G:\My Drive"));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(id: &str) -> ItemDraft {
        ItemDraft {
            id: id.into(),
            title: format!("Do {id}"),
            why: None,
            minutes: None,
            priority: Default::default(),
            due: None,
            after: vec![],
            steps: vec![StepDraft {
                id: None,
                text: "Open it".into(),
                url: Some("https://example.com/a".into()),
                copy: None,
                file: None,
            }],
            ask: None,
            waiting: None,
            reopen: false,
            owner: Default::default(),
            done: None,
        }
    }

    fn additions(items: Vec<ItemDraft>) -> Additions {
        Additions {
            title: None,
            summary: None,
            theme: None,
            section: None,
            items,
        }
    }

    #[test]
    fn what_an_agent_writes_is_checked_before_it_is_kept() {
        let none = |_: &mut String| {};
        let mut ok = additions(vec![draft("grant-api")]);
        clean(&mut ok, &none).unwrap();
        assert_eq!(ok.items[0].steps[0].id.as_deref(), Some("1"));
        for bad in [
            {
                let mut d = draft("Grant API");
                d.title = "x".into();
                d
            },
            {
                let mut d = draft("a");
                d.steps[0].url = Some("http://example.com".into());
                d
            },
            {
                let mut d = draft("a");
                d.steps[0].url = Some("javascript:alert(1)".into());
                d
            },
            {
                let mut d = draft("a");
                d.due = Some("2026-13-01".into());
                d
            },
            {
                let mut d = draft("a");
                d.after = vec!["a".into()];
                d
            },
        ] {
            assert!(clean(&mut additions(vec![bad]), &none).is_err());
        }
        let twice = additions(vec![draft("a"), draft("a")]);
        assert!(clean(&mut { twice }, &none).is_err());
        // A value to copy that holds a secret is refused, not cut.
        let secret = |t: &mut String| *t = t.replace("sk-123", "[hidden]");
        let mut d = draft("a");
        d.steps[0].copy = Some("export KEY=sk-123".into());
        assert!(clean(&mut additions(vec![d]), &secret).is_err());
        let mut d = draft("a");
        d.title = "Paste sk-123".into();
        let mut add = additions(vec![d]);
        clean(&mut add, &secret).unwrap();
        assert_eq!(add.items[0].title, "Paste [hidden]");
    }

    #[test]
    fn whose_an_item_is_decides_what_it_may_hold() {
        let mut list = Checklist {
            project_id: ProjectId::new(),
            title: "T".into(),
            summary: None,
            theme: None,
            sections: vec![],
            notes: vec![],
            task_id: None,
            created_at: Timestamp(0),
            updated_at: Timestamp(0),
            revision: 0,
        };
        let mut bare = draft("a");
        bare.steps.clear();
        assert!(owned(&list, std::slice::from_ref(&bare)).is_err());
        bare.owner = Some(ChecklistOwner::Agent);
        assert!(owned(&list, std::slice::from_ref(&bare)).is_ok());
        // Left out, a new item is the person's and a rewrite keeps its owner.
        let mut done = draft("a");
        done.done = Some(true);
        assert!(owned(&list, std::slice::from_ref(&done)).is_err());
        merge(
            &mut list,
            None,
            &additions(vec![bare]),
            &HashMap::new(),
            Timestamp(1),
        );
        let settled = owned(&list, std::slice::from_ref(&done)).unwrap();
        assert_eq!(settled[0].owner, Some(ChecklistOwner::Agent));
        done.owner = Some(ChecklistOwner::Person);
        assert!(owned(&list, &[done]).is_err());
    }

    #[test]
    fn a_rewritten_item_keeps_its_ticks_where_it_was() {
        let task = TaskId::new();
        let mut list = Checklist {
            project_id: ProjectId::new(),
            title: "T".into(),
            summary: None,
            theme: None,
            sections: vec![],
            notes: vec![],
            task_id: None,
            created_at: Timestamp(0),
            updated_at: Timestamp(0),
            revision: 0,
        };
        let none = |_: &mut String| {};
        let mut add = additions(vec![draft("a"), draft("b")]);
        clean(&mut add, &none).unwrap();
        merge(&mut list, Some(task), &add, &HashMap::new(), Timestamp(1));
        assert_eq!(list.sections[0].title, "To do");
        list.item_mut("a").unwrap().steps[0].done_at = Some(Timestamp(2));
        let mut again = draft("a");
        again.title = "Do a better".into();
        again.steps.push(StepDraft {
            id: None,
            text: "Then this".into(),
            url: None,
            copy: Some("ophio setup".into()),
            file: None,
        });
        let mut add = additions(vec![again]);
        clean(&mut add, &none).unwrap();
        merge(&mut list, Some(task), &add, &HashMap::new(), Timestamp(3));
        let ids: Vec<&str> = list.items().map(|i| i.item_id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        let a = list.item("a").unwrap();
        assert_eq!(a.title, "Do a better");
        assert_eq!(a.steps[0].done_at, Some(Timestamp(2)));
        assert_eq!(a.steps[1].done_at, None);
        assert_eq!(a.added_at, Timestamp(1));
        // Named, a section is made, and an item moves into it.
        let mut add = additions(vec![draft("b")]);
        add.section = Some("Before launch".into());
        clean(&mut add, &none).unwrap();
        merge(&mut list, Some(task), &add, &HashMap::new(), Timestamp(4));
        assert_eq!(list.sections.len(), 2);
        assert_eq!(list.sections[1].section_id, "before-launch");
        assert_eq!(list.sections[1].items[0].item_id, "b");
    }

    #[test]
    fn items_cannot_wait_on_each_other() {
        let task = TaskId::new();
        let mut list = Checklist {
            project_id: ProjectId::new(),
            title: "T".into(),
            summary: None,
            theme: None,
            sections: vec![],
            notes: vec![],
            task_id: None,
            created_at: Timestamp(0),
            updated_at: Timestamp(0),
            revision: 0,
        };
        let mut a = draft("a");
        a.after = vec!["b".into()];
        let mut b = draft("b");
        b.after = vec!["a".into()];
        let add = additions(vec![a, b]);
        merge(&mut list, Some(task), &add, &HashMap::new(), Timestamp(1));
        assert!(check(&list, &add.items).is_err());
        let mut c = draft("c");
        c.after = vec!["gone".into()];
        let add = additions(vec![c]);
        let mut fresh = list.clone();
        fresh.sections.clear();
        merge(&mut fresh, Some(task), &add, &HashMap::new(), Timestamp(1));
        assert!(check(&fresh, &add.items).is_err());
    }

    #[test]
    fn names_from_agents_are_safe_anywhere() {
        assert_eq!(plain_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(plain_name(".hidden"), "hidden");
        assert_eq!(plain_name("a\u{202e}b.pdf"), "a_b.pdf");
        assert_eq!(plain_name(""), "file");
        assert_eq!(
            taildrop_names("100.1.2.3\tgalaxy\n100.1.2.4\tlaptop\toffline; last seen 2h ago\n"),
            ["galaxy", "laptop"]
        );
        assert!(day("2026-10-17") && !day("2026-1-17") && !day("2026-10-32"));
        assert!(colour("#A3e635") && !colour("a3e635") && !colour("#a3e63"));
    }
}
