// SPDX-License-Identifier: Apache-2.0
//! A project's checklist: what the person has to do themselves for the
//! project's work to go on, written by its agents.
//!
//! Everything an item needs is in it: its steps in order, each with the link
//! to open, the value to copy or the file to take, so nothing sends the
//! person to a chat or to a file kept somewhere else. The person ticks steps
//! and items off, answers what an item asks and asks about it in turn. An
//! item an agent waits on starts that agent again the moment it is done.
//! Items go by priority first and by the day suggested for them second.
//! An item can also be an agent's own work that waits on the person's
//! items: the agent keeps it, and the person sees it and sends feedback on
//! it. Advertised as `checklist_v1`, agents' own items as
//! `checklist_agent_items_v1`.
use crate::{ProjectId, TaskId, time::Timestamp};
use serde::{Deserialize, Serialize};
use std::{cmp::Reverse, collections::HashMap};

/// The most sections in a checklist.
pub const MAX_SECTIONS: usize = 24;
/// The most items in a checklist, over all its sections.
pub const MAX_ITEMS: usize = 160;
/// The most steps in an item.
pub const MAX_STEPS: usize = 30;
/// The most options a question offers.
pub const MAX_OPTIONS: usize = 8;
/// The most notes kept with an item, or with the whole checklist: the oldest
/// go first.
pub const MAX_NOTES: usize = 40;
/// The most files a checklist carries.
pub const MAX_FILES: usize = 60;
/// The largest file a checklist carries.
pub const MAX_FILE: u64 = 25 * 1024 * 1024;
/// The longest id an agent gives an item or a step.
pub const MAX_ID: usize = 48;
/// The longest title of a checklist, section or item.
pub const MAX_TITLE: usize = 120;
/// The longest step, note, answer or line of text.
pub const MAX_TEXT: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Checklist {
    pub project_id: ProjectId,
    pub title: String,
    /// What it is for, in a line or two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    /// The project's look. Absent, it wears Øphio's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub theme: Option<ChecklistTheme>,
    pub sections: Vec<ChecklistSection>,
    /// Questions about the checklist as a whole, and the answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ChecklistNote>>", optional)]
    pub notes: Vec<ChecklistNote>,
    /// The task whose agent last wrote it, told of questions about the whole
    /// checklist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_id: Option<TaskId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
}

/// How a checklist looks. The phone keeps only colours it can read text on,
/// and falls back to Øphio's for the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistTheme {
    /// Whose look it is, such as the project's brand name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// The brand colour, `#rrggbb`: ticks, the next item and links.
    pub accent: String,
    /// A background colour, `#rrggbb`. Absent, the phone's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub font: Option<ChecklistFont>,
    /// The brand's logo, shown small at the top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub logo: Option<ChecklistFile>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChecklistFont {
    Sans,
    Serif,
    Rounded,
    Mono,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistSection {
    pub section_id: String,
    pub title: String,
    pub items: Vec<ChecklistItem>,
}

/// One thing for the person to do, with everything it takes, or an
/// agent's own work that waits on what the person does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistItem {
    /// Chosen by the agent, so it can change the item later.
    pub item_id: String,
    pub title: String,
    /// Whose it is to do. Absent, the person's.
    #[serde(default, skip_serializing_if = "ChecklistOwner::is_person")]
    #[ts(as = "Option<ChecklistOwner>", optional)]
    pub owner: ChecklistOwner,
    /// Why it matters, or what it lets go on, in a line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub why: Option<String>,
    /// About how long it takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub minutes: Option<u32>,
    /// How much it matters: what the checklist goes by first.
    #[serde(default)]
    pub priority: ChecklistPriority,
    /// The day it is suggested for, `YYYY-MM-DD`: what the checklist goes by
    /// after priority. Only a suggestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub due: Option<String>,
    /// Items to finish first, by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub after: Vec<String>,
    pub steps: Vec<ChecklistStep>,
    /// What the item asks the person to choose or tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ask: Option<ChecklistAsk>,
    /// What the agent does once the item is done. Present, the agent waits
    /// on it, and Ophio starts it again the moment the item is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub waiting: Option<String>,
    /// The task whose agent wrote the item, told when it is done, answered
    /// or asked about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ChecklistNote>>", optional)]
    pub notes: Vec<ChecklistNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub done_at: Option<Timestamp>,
    pub added_at: Timestamp,
    pub changed_at: Timestamp,
}

/// Whose an item is to do: the person's, which they tick off, or an
/// agent's own work, which the agent keeps and the person only sees and
/// sends feedback on.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChecklistOwner {
    #[default]
    Person,
    Agent,
}

impl ChecklistOwner {
    pub fn is_person(&self) -> bool {
        *self == Self::Person
    }
}

/// How much an item matters, the least first.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChecklistPriority {
    Low,
    #[default]
    Medium,
    High,
    /// Important work is stopped until it is done.
    Blocker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistStep {
    pub step_id: String,
    pub text: String,
    /// The page to open for it: `https:`, `mailto:` or `tel:`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    /// An exact value to copy, such as a command, a name or a setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub copy: Option<String>,
    /// A file the step needs, kept with the checklist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub file: Option<ChecklistFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub done_at: Option<Timestamp>,
}

/// A file kept with a checklist, by its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistFile {
    pub name: String,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: u64,
    pub sha256: String,
    pub media_type: String,
}

/// A question an item asks the person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistAsk {
    pub question: String,
    /// What the person can choose from. Empty, they answer in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ChecklistOption>>", optional)]
    pub options: Vec<ChecklistOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<ChecklistAnswer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistOption {
    pub label: String,
    /// What choosing it means. `description` is taken too, as questions
    /// elsewhere name it.
    #[serde(
        default,
        alias = "description",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistAnswer {
    pub text: String,
    pub at: Timestamp,
}

/// A question or remark from the person, or an agent's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistNote {
    pub note_id: String,
    pub from: NoteFrom,
    pub text: String,
    pub at: Timestamp,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NoteFrom {
    Person,
    Agent,
}

/// A project's checklist at a glance, for the row that opens it. Its
/// counts and next item are the person's items alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecklistSummary {
    pub project_id: ProjectId,
    pub title: String,
    pub open: u32,
    pub done: u32,
    /// Open items an agent waits on.
    pub waiting: u32,
    /// Open items that are blockers.
    pub blockers: u32,
    /// Questions from the person not yet answered, under any item but an
    /// agent's own item that no task keeps now, which nobody is told of.
    pub unanswered: u32,
    /// Agents' own items still open.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[ts(as = "Option<u32>", optional)]
    pub for_agent: u32,
    /// The person's next item to do: the most pressing open one with
    /// nothing before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub accent: Option<String>,
    pub updated_at: Timestamp,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
}

impl ChecklistItem {
    pub fn is_done(&self) -> bool {
        self.done_at.is_some()
    }

    /// Whether it is an agent's own work rather than the person's.
    pub fn for_agent(&self) -> bool {
        self.owner == ChecklistOwner::Agent
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl ChecklistPriority {
    /// As agents write it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Blocker => "blocker",
        }
    }
}

impl Checklist {
    pub fn items(&self) -> impl Iterator<Item = &ChecklistItem> {
        self.sections.iter().flat_map(|s| s.items.iter())
    }

    pub fn item(&self, item_id: &str) -> Option<&ChecklistItem> {
        self.items().find(|i| i.item_id == item_id)
    }

    pub fn item_mut(&mut self, item_id: &str) -> Option<&mut ChecklistItem> {
        self.sections
            .iter_mut()
            .flat_map(|s| s.items.iter_mut())
            .find(|i| i.item_id == item_id)
    }

    /// Whether `item` can be done now: every item it comes after is done,
    /// or no longer on the checklist.
    pub fn ready(&self, item: &ChecklistItem) -> bool {
        item.after
            .iter()
            .all(|a| self.item(a).is_none_or(ChecklistItem::is_done))
    }

    /// The person's most pressing open item that is ready, as `sort` weighs
    /// items, whatever section it is in; of those as pressing, the first.
    /// The phone also lifts an item whose day has come by the person's own
    /// day, which the computer can't know, so near midnight the two can
    /// differ.
    pub fn next(&self) -> Option<&ChecklistItem> {
        let (keys, _) = self.pressing();
        (self.items().enumerate())
            .filter(|(_, i)| !i.for_agent() && !i.is_done() && self.ready(i))
            .min_by_key(|(at, i)| (keys[&i.item_id].clone(), *at))
            .map(|(_, i)| i)
    }

    /// Puts the most pressing first. Items go by priority, then by the day
    /// suggested for them, the earliest first and those with none last, then
    /// ahead of what comes after them, then as the agents wrote them. An item
    /// others come after counts as pressing, and as soon, as the most
    /// pressing of those. Sections go by their most pressing item.
    pub fn sort(&mut self) {
        let (keys, depth) = self.pressing();
        let order = |i: &ChecklistItem| {
            let at = depth.get(&i.item_id).copied().unwrap_or(0);
            (keys[&i.item_id].clone(), at)
        };
        for section in &mut self.sections {
            section.items.sort_by_cached_key(order);
        }
        (self.sections).sort_by_cached_key(|s| (s.items.is_empty(), s.items.first().map(order)));
    }

    /// How pressing each item is, lifted to the most pressing of those that
    /// come after it, and how many items come before it.
    fn pressing(&self) -> (HashMap<String, Urgency>, HashMap<String, usize>) {
        let edges: Vec<(String, String)> = (self.items())
            .flat_map(|i| i.after.iter().map(|a| (i.item_id.clone(), a.clone())))
            .collect();
        let mut keys: HashMap<String, Urgency> = (self.items())
            .map(|i| (i.item_id.clone(), urgency(i)))
            .collect();
        let mut depth: HashMap<String, usize> = HashMap::new();
        // Each round passes both one item further along; a checklist has no
        // loops, so as many rounds as items settle them.
        for _ in 0..keys.len() {
            let mut changed = false;
            for (item, first) in &edges {
                let (Some(theirs), Some(mine)) = (keys.get(item).cloned(), keys.get_mut(first))
                else {
                    continue;
                };
                if theirs < *mine {
                    *mine = theirs;
                    changed = true;
                }
                let deeper = depth.get(first).copied().unwrap_or(0) + 1;
                if deeper > depth.get(item).copied().unwrap_or(0) {
                    depth.insert(item.clone(), deeper);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        (keys, depth)
    }

    /// Every file the checklist carries, the logo too.
    pub fn files(&self) -> impl Iterator<Item = &ChecklistFile> {
        let logo = self.theme.as_ref().and_then(|t| t.logo.as_ref());
        logo.into_iter().chain(
            self.items()
                .flat_map(|i| i.steps.iter().filter_map(|s| s.file.as_ref())),
        )
    }

    pub fn summary(&self) -> ChecklistSummary {
        let (mut open, mut done, mut waiting, mut blockers, mut unanswered) = (0, 0, 0, 0, 0);
        let mut for_agent = 0;
        for item in self.items() {
            if !(item.for_agent() && item.task_id.is_none()) {
                unanswered += unanswered_in(&item.notes);
            }
            if item.for_agent() {
                for_agent += u32::from(!item.is_done());
            } else if item.is_done() {
                done += 1;
            } else {
                open += 1;
                waiting += u32::from(item.waiting.is_some());
                blockers += u32::from(item.priority == ChecklistPriority::Blocker);
            }
        }
        unanswered += unanswered_in(&self.notes);
        ChecklistSummary {
            project_id: self.project_id,
            title: self.title.clone(),
            open,
            done,
            waiting,
            blockers,
            unanswered,
            for_agent,
            next: self.next().map(|i| i.title.clone()),
            accent: self.theme.as_ref().map(|t| t.accent.clone()),
            updated_at: self.updated_at,
            revision: self.revision,
        }
    }
}

/// How pressing an item is, the most pressing least: its priority, then
/// whether it has a day suggested, then the day.
type Urgency = (Reverse<ChecklistPriority>, bool, Option<String>);

fn urgency(item: &ChecklistItem) -> Urgency {
    let due = item.due.clone();
    (Reverse(item.priority), due.is_none(), due)
}

/// Whether the person's last word in `notes` is still to be answered.
fn unanswered_in(notes: &[ChecklistNote]) -> u32 {
    u32::from(notes.last().is_some_and(|n| n.from == NoteFrom::Person))
}

/// What an agent writes for an item: its new whole content. Ticks, answers
/// and notes carry over from the item with the same id, and a step's tick
/// from the step with the same id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemDraft {
    pub id: String,
    pub title: String,
    /// `agent` for the agent's own work, which waits on the person's items
    /// it comes after. Absent, a rewrite keeps the item's owner and a new
    /// item is the person's.
    #[serde(default)]
    pub owner: Option<ChecklistOwner>,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub minutes: Option<u32>,
    #[serde(default)]
    pub priority: ChecklistPriority,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub steps: Vec<StepDraft>,
    #[serde(default)]
    pub ask: Option<AskDraft>,
    #[serde(default)]
    pub waiting: Option<String>,
    /// Opens a done item again, its ticks cleared, for the person to do
    /// over.
    #[serde(default)]
    pub reopen: bool,
    /// On an agent's own item, `true` marks it and its steps done and
    /// `false` opens it again. Absent, it stays as it was. Only the person
    /// ticks off their own items.
    #[serde(default)]
    pub done: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepDraft {
    /// Absent, the step's place: `1`, `2` and on.
    #[serde(default)]
    pub id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub copy: Option<String>,
    /// A file in the project, or in the task's scratch folder, copied into
    /// the checklist now.
    #[serde(default)]
    pub file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskDraft {
    pub question: String,
    #[serde(default)]
    pub options: Vec<ChecklistOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThemeDraft {
    #[serde(default)]
    pub name: Option<String>,
    pub accent: String,
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default)]
    pub font: Option<ChecklistFont>,
    /// The logo, a file in the project.
    #[serde(default)]
    pub logo: Option<String>,
}

/// Where the computer puts a checklist's files for the person.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum FileDelivery {
    /// The phone that asks, through Transfers.
    #[default]
    Phone,
    /// A device on the person's tailnet, by Taildrop.
    Taildrop { device: String },
    /// A folder on the computer, such as one a drive app keeps in sync. Each
    /// project's files go in a folder of its own inside it.
    Folder { path: String },
    /// A cloud drive set up in rclone: `remote` as `rclone listremotes`
    /// names it, and the folder in it.
    Drive { remote: String, folder: String },
}

#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct FileDeliverySettings {
    pub delivery: FileDelivery,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
}

/// The person's choice, and what the computer could deliver to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct FileDeliveryView {
    pub settings: FileDeliverySettings,
    /// Devices Taildrop can send to; absent without Tailscale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub taildrop: Option<Vec<String>>,
    /// Folders found on the computer that drive apps keep in sync, and the
    /// downloads folder.
    pub folders: Vec<DeliveryFolder>,
    /// Drives set up in rclone; absent without rclone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub drives: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct DeliveryFolder {
    /// What it is, such as "Google Drive" or "Downloads".
    pub label: String,
    pub path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, after: &[&str], done: bool) -> ChecklistItem {
        ChecklistItem {
            item_id: id.into(),
            title: id.to_uppercase(),
            owner: ChecklistOwner::Person,
            why: None,
            minutes: None,
            priority: ChecklistPriority::Medium,
            due: None,
            after: after.iter().map(|a| a.to_string()).collect(),
            steps: vec![],
            ask: None,
            waiting: (id == "c").then(|| "deploy".into()),
            task_id: None,
            notes: vec![],
            done_at: done.then_some(Timestamp(1)),
            added_at: Timestamp(0),
            changed_at: Timestamp(0),
        }
    }

    fn list(sections: Vec<(&str, Vec<ChecklistItem>)>) -> Checklist {
        Checklist {
            project_id: ProjectId::new(),
            title: "Launch".into(),
            summary: None,
            theme: None,
            sections: (sections.into_iter())
                .map(|(id, items)| ChecklistSection {
                    section_id: id.into(),
                    title: id.to_uppercase(),
                    items,
                })
                .collect(),
            notes: vec![],
            task_id: None,
            created_at: Timestamp(0),
            updated_at: Timestamp(0),
            revision: 1,
        }
    }

    fn with(
        mut item: ChecklistItem,
        priority: ChecklistPriority,
        due: Option<&str>,
    ) -> ChecklistItem {
        item.priority = priority;
        item.due = due.map(Into::into);
        item
    }

    fn order(list: &Checklist) -> Vec<Vec<&str>> {
        (list.sections.iter())
            .map(|s| s.items.iter().map(|i| i.item_id.as_str()).collect())
            .collect()
    }

    #[test]
    fn items_go_by_priority_then_the_day_suggested() {
        use ChecklistPriority::*;
        let mut list = list(vec![(
            "one",
            vec![
                with(item("a", &[], false), Low, Some("2026-10-01")),
                with(item("b", &[], false), Medium, Some("2026-12-01")),
                with(item("d", &[], false), Medium, None),
                with(item("e", &[], false), Medium, Some("2026-11-01")),
                with(item("f", &[], false), High, None),
                with(item("g", &[], false), Blocker, None),
            ],
        )]);
        list.sort();
        assert_eq!(order(&list), [["g", "f", "e", "b", "d", "a"]]);
        assert_eq!(list.summary().blockers, 1);
    }

    #[test]
    fn what_comes_before_a_pressing_item_goes_with_it() {
        use ChecklistPriority::*;
        // "x" is a blocker that needs "w", which needs "v": both go first, in
        // the order they are done. "y" is high in another section and needs
        // nothing; "z" there is low.
        let mut list = list(vec![
            (
                "later",
                vec![
                    with(item("z", &[], false), Low, None),
                    with(item("y", &[], false), High, None),
                ],
            ),
            (
                "first",
                vec![
                    with(item("u", &[], false), Medium, None),
                    with(item("x", &["w"], false), Blocker, None),
                    with(item("w", &["v"], false), Low, None),
                    with(item("v", &[], false), Low, None),
                ],
            ),
        ]);
        list.sort();
        assert_eq!(order(&list), [vec!["v", "w", "x", "u"], vec!["y", "z"]]);
        assert_eq!(list.next().map(|i| i.item_id.as_str()), Some("v"));
        // Sorting again changes nothing.
        let before = list.clone();
        list.sort();
        assert_eq!(list, before);
    }

    #[test]
    fn the_next_item_is_the_first_open_one_with_nothing_before_it() {
        let mut list = list(vec![(
            "one",
            vec![
                item("a", &[], true),
                item("b", &["z"], false),
                item("c", &["b"], false),
            ],
        )]);
        // An item it comes after that has gone holds nothing up.
        assert_eq!(list.next().map(|i| i.item_id.as_str()), Some("b"));
        list.item_mut("b").unwrap().after = vec!["c".into()];
        list.item_mut("c").unwrap().after = vec![];
        assert_eq!(list.next().map(|i| i.item_id.as_str()), Some("c"));
        let s = list.summary();
        assert_eq!((s.open, s.done, s.waiting, s.unanswered), (2, 1, 1, 0));
        assert_eq!(s.next.as_deref(), Some("C"));
    }

    #[test]
    fn the_next_item_is_the_most_pressing_ready_one_in_any_section() {
        use ChecklistPriority::*;
        // A done blocker keeps "one" first, but "b" in "two" is more pressing
        // than "a", which is ready before it.
        let mut list = list(vec![
            (
                "one",
                vec![
                    with(item("x", &[], true), Blocker, None),
                    with(item("a", &[], false), Medium, None),
                ],
            ),
            ("two", vec![with(item("b", &[], false), High, None)]),
        ]);
        list.sort();
        assert_eq!(order(&list), [vec!["x", "a"], vec!["b"]]);
        assert_eq!(list.next().map(|i| i.item_id.as_str()), Some("b"));
        assert_eq!(list.summary().next.as_deref(), Some("B"));
        // An item a blocker comes after is as pressing as the blocker.
        list.sections[1]
            .items
            .push(with(item("y", &["z"], false), Blocker, None));
        list.sections[1]
            .items
            .push(with(item("z", &[], false), Low, None));
        assert_eq!(list.next().map(|i| i.item_id.as_str()), Some("z"));
        // As pressing, the first goes.
        list.item_mut("z").unwrap().done_at = Some(Timestamp(2));
        list.item_mut("y").unwrap().done_at = Some(Timestamp(2));
        list.item_mut("a").unwrap().priority = High;
        assert_eq!(list.next().map(|i| i.item_id.as_str()), Some("a"));
    }

    #[test]
    fn an_agents_own_items_are_counted_apart_from_the_persons() {
        let mut deploy = item("deploy", &["a"], false);
        deploy.owner = ChecklistOwner::Agent;
        deploy.priority = ChecklistPriority::Blocker;
        let mut shipped = item("ship", &[], true);
        shipped.owner = ChecklistOwner::Agent;
        let mut list = list(vec![(
            "one",
            vec![deploy, shipped, item("a", &[], false), item("b", &[], true)],
        )]);
        // The person's counts leave the agent's work out.
        let s = list.summary();
        assert_eq!(
            (s.open, s.done, s.blockers, s.for_agent),
            (1, 1, 0, 1),
            "{s:?}"
        );
        assert_eq!(s.next.as_deref(), Some("A"));
        list.item_mut("a").unwrap().done_at = Some(Timestamp(2));
        let s = list.summary();
        assert_eq!((s.open, s.for_agent, s.next), (0, 1, None));

        // The person's is the default, and isn't written out.
        let mut json = serde_json::to_value(list.item("a").unwrap()).unwrap();
        assert!(json.get("owner").is_none(), "{json}");
        let agent = serde_json::to_value(list.item("deploy").unwrap()).unwrap();
        assert_eq!(agent["owner"], "agent");
        let back: ChecklistItem = serde_json::from_value(agent).unwrap();
        assert!(back.for_agent());
        json.as_object_mut().unwrap().remove("owner");
        let back: ChecklistItem = serde_json::from_value(json).unwrap();
        assert_eq!(back.owner, ChecklistOwner::Person);
        let mut none = list.clone();
        none.sections[0].items.retain(|i| !i.for_agent());
        let summary = serde_json::to_value(none.summary()).unwrap();
        assert!(summary.get("for_agent").is_none(), "{summary}");
        let draft: ItemDraft = serde_json::from_value(serde_json::json!({
            "id": "deploy", "title": "Deploy", "owner": "agent", "done": true
        }))
        .unwrap();
        assert_eq!(
            (draft.owner, draft.done),
            (Some(ChecklistOwner::Agent), Some(true))
        );
        // Feedback on an agent's item no task keeps is told to nobody, so
        // it isn't counted as waiting for an answer.
        let note = ChecklistNote {
            note_id: "n".into(),
            from: NoteFrom::Person,
            text: "Use the staging server".into(),
            at: Timestamp(3),
        };
        list.item_mut("deploy").unwrap().notes.push(note.clone());
        assert_eq!(list.summary().unanswered, 0);
        list.item_mut("deploy").unwrap().task_id = Some(TaskId::new());
        assert_eq!(list.summary().unanswered, 1);
        list.item_mut("a").unwrap().notes.push(note);
        assert_eq!(list.summary().unanswered, 2);
    }
}
