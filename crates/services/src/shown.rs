// SPDX-License-Identifier: Apache-2.0
//! What an agent puts in front of the person through Ophio: a picture or video
//! from its project's folder, a page it serves on this computer, or an
//! invitation to do something in the app. Each lands in Talk as a timeline
//! item; nothing here waits for the person.
use crate::{
    CompanionHub,
    commands::{emit_previews, valid_label},
    error, internal,
    transfers::{open_project_file, open_scratch_file},
};
use ports::bridge::PersonCall;
use protocol::{auth::Principal, model::*, *};
use std::{
    io::Read,
    net::IpAddr,
    path::{Component, Path},
};

/// The most one shown file may be.
pub(crate) const MAX_SHOWN: u64 = 20 * 1024 * 1024;
/// Previews a project may have before an agent can add another, or another
/// is found.
pub(crate) const MAX_PREVIEWS: usize = 32;
/// The longest command a preview's server is started with, in characters.
const MAX_START: usize = 1000;
/// The longest preview address an agent may give, and how many times over
/// its escapes are read: past either it is refused, not read.
pub(crate) const MAX_ADDRESS: usize = 8 * 1024;
const MAX_READINGS: usize = 4;

impl CompanionHub {
    pub(crate) async fn person_request(
        &self,
        run_id: RunId,
        task: &Task,
        call: PersonCall,
    ) -> Result<String, ProtocolError> {
        // What the person reads is hidden as everything else the run says.
        let hide = self.person_masking(run_id)?;
        match call {
            PersonCall::Show {
                path,
                mut caption,
                invite,
            } => {
                let project: Project = self.read(|r| r.require("project", task.project_id))?;
                let scratch = self.scratch_dir(task.task_id);
                let (folder, path) = shown_relative(&project, &scratch, &path)?;
                let file = match folder {
                    Folder::Project => open_project_file(&project, &path)?,
                    Folder::Scratch => open_scratch_file(&scratch, &path)?,
                };
                let bytes = tokio::task::spawn_blocking(move || {
                    let mut bytes = Vec::new();
                    file.take(MAX_SHOWN + 1)
                        .read_to_end(&mut bytes)
                        .map(|_| bytes)
                })
                .await
                .map_err(|_| internal("Reading the file stopped"))?
                .map_err(internal)?;
                if bytes.len() as u64 > MAX_SHOWN {
                    return Err(error(
                        ErrorCode::LimitExceeded,
                        "That file is larger than 20 MiB; show a smaller export of it",
                    ));
                }
                let media_type = sniff(&bytes).ok_or_else(|| {
                    error(
                        ErrorCode::InvalidRequest,
                        "Only a PNG, JPEG, WebP or GIF picture or an MP4, WebM or MOV video can be shown",
                    )
                })?;
                if invite == Some(MediaInvite::Talk) && !media_type.starts_with("video/") {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "invite \"talk\" is for a video; to have them point at a picture, invite \"draw\"",
                    ));
                }
                if self.run_stopped(run_id)? {
                    return Err(error(ErrorCode::Expired, "Run is no longer active"));
                }
                let link = ArtifactLink {
                    task_id: task.task_id,
                    run_id: Some(run_id),
                    annotation: None,
                    role: ArtifactRole::Shown,
                };
                self.read(|r| crate::results::validate_link(r, &link))?;
                caption.iter_mut().for_each(&hide);
                let mut shown_path = path.clone();
                hide(&mut shown_path);
                let name = (shown_path.rsplit('/').next()).unwrap_or(&shown_path);
                let meta = self
                    .store_artifact(
                        &bytes,
                        media_type,
                        Some(link),
                        Principal::System,
                        None,
                        Some(name.to_owned()),
                    )
                    .await?;
                let reply = format!(
                    "Shown to the person in Chat: {path} ({media_type}).{}",
                    match invite {
                        Some(MediaInvite::Draw) =>
                            " Chat asks them to draw on it; what they mark comes back as their next message.",
                        Some(MediaInvite::Talk) =>
                            " Chat asks them to watch it and talk over it; what they say comes back as their next message, timed against the video.",
                        None => "",
                    }
                );
                self.show(
                    run_id,
                    task,
                    Shown::Media {
                        artifact_id: meta.artifact_id,
                        media_type: media_type.into(),
                        path: shown_path,
                        caption,
                        invite,
                    },
                )?;
                Ok(reply)
            }
            PersonCall::Preview {
                url,
                label,
                mut note,
                start,
                mut questions,
            } => {
                let url = loopback(&url)?;
                // The address is opened as given, so it can't be hidden in.
                spelled_plainly(&url, &hide)?;
                let start = start_command(start, &hide)?;
                let mut label = label.unwrap_or_else(|| "Preview".into());
                hide(&mut label);
                valid_label(&label)?;
                note.iter_mut().for_each(&hide);
                for q in &mut questions {
                    hide(&mut q.question);
                    q.header.iter_mut().for_each(&hide);
                    for o in &mut q.options {
                        hide(&mut o.label);
                        o.description.iter_mut().for_each(&hide);
                    }
                }
                let asked = questions.len();
                if self.run_stopped(run_id)? {
                    return Err(error(ErrorCode::Expired, "Run is no longer active"));
                }
                let now = self.inner.clock.now();
                let sandboxed = (self.inner.sandbox_runs.lock())
                    .map_err(internal)?
                    .contains_key(&run_id);
                let (preview_id, reply) = self.write(|tx| {
                    let previews: Vec<PreviewEndpoint> = tx
                        .read()
                        .list::<PreviewEndpoint>("preview")?
                        .into_iter()
                        .filter(|p| p.project_id == task.project_id)
                        .collect();
                    let preview = match previews.iter().find(|p| crate::preview::same_page(&p.url, &url)) {
                        // One found by itself becomes the agent's, kept while
                        // its server is stopped so it can be started again.
                        Some(p) if start.is_some() && p.start != start => {
                            let p = PreviewEndpoint {
                                start: start.clone(),
                                automatic: false,
                                ..p.clone()
                            };
                            tx.put("preview", p.preview_id, &p)?;
                            emit_previews(tx, task.project_id)?;
                            crate::expiry::page_seen(tx, p.preview_id, true, now)?;
                            p
                        }
                        // Shown again, one the agent showed stays a week longer.
                        Some(p) => {
                            crate::expiry::page_seen(tx, p.preview_id, false, now)?;
                            p.clone()
                        }
                        None if previews.len() >= MAX_PREVIEWS => {
                            return Err(error(
                                ErrorCode::LimitExceeded,
                                "This project has as many previews as it can keep; the person can remove some in Control",
                            )
                            .into());
                        }
                        None => {
                            let p = PreviewEndpoint {
                                preview_id: PreviewId::new(),
                                project_id: task.project_id,
                                label: label.clone(),
                                url: url.clone(),
                                reachable: None,
                                checked_at: None,
                                automatic: false,
                                start: start.clone(),
                            };
                            tx.put("preview", p.preview_id, &p)?;
                            emit_previews(tx, task.project_id)?;
                            crate::expiry::page_seen(tx, p.preview_id, true, now)?;
                            p
                        }
                    };
                    // One already listed may have been named by its page.
                    let mut label = preview.label;
                    hide(&mut label);
                    append(
                        tx,
                        now,
                        run_id,
                        task.task_id,
                        Shown::Preview {
                            preview_id: preview.preview_id,
                            label: label.clone(),
                            url: preview.url,
                            note,
                            questions,
                        },
                    )?;
                    let mut shown = format!(
                        "Shown to the person in Chat as the preview “{label}”, also listed with the project's previews in Control. They open it on their phone through Ophio."
                    );
                    if asked > 0 {
                        shown.push_str(match asked {
                            1 => " It asks them your question beside the page; their answer comes back as their next message, whenever they send it, so end your turn rather than wait.",
                            _ => " It asks them your questions beside the page; their answers come back together as their next message, whenever they send them, so end your turn rather than wait.",
                        });
                    }
                    if sandboxed {
                        shown.push_str(" Ophio reaches your server on this chat's own loopback, inside its sandbox, while your turn lasts.");
                    }
                    let reply = match preview.start {
                        Some(start) => format!(
                            "{shown} Your server stops when your turn ends; when they open the preview and nothing answers, Ophio offers to start it again with `{start}` in the project's folder."
                        ),
                        None => format!(
                            "{shown} Your server stops when your turn ends, so it won't answer once you're done: show it again with start, the command that starts it from the project's folder, so Ophio can start it again for them."
                        ),
                    };
                    Ok((preview.preview_id, reply))
                })?;
                let mut runs = self.inner.preview_runs.lock().map_err(internal)?;
                match sandboxed {
                    true => runs.insert(preview_id, run_id),
                    false => runs.remove(&preview_id),
                };
                Ok(reply)
            }
            PersonCall::Invite {
                action,
                mut note,
                mut goal,
            } => {
                if self.run_stopped(run_id)? {
                    return Err(error(ErrorCode::Expired, "Run is no longer active"));
                }
                hide(&mut note);
                goal.iter_mut().for_each(&hide);
                self.show(run_id, task, Shown::Invitation { action, note, goal })?;
                Ok("Shown to the person in Chat with a button for it. Nothing waits for it: whatever they do comes back as their next message.".into())
            }
            PersonCall::Wake {
                minutes,
                mut reason,
            } => {
                hide(&mut reason);
                self.wake_me(run_id, task, minutes, reason)
            }
            PersonCall::CanAsk => self.can_ask(),
            PersonCall::ChecklistRead => self.checklist_read(run_id, task),
            PersonCall::ChecklistAdd {
                title,
                summary,
                theme,
                section,
                items,
            } => {
                let add = crate::checklist::Additions {
                    title,
                    summary,
                    theme,
                    section,
                    items,
                };
                self.checklist_add(run_id, task, add, &hide).await
            }
            PersonCall::ChecklistRemove { items } => self.checklist_remove(run_id, task, items),
            PersonCall::ChecklistReply { item, text } => {
                self.checklist_reply(run_id, task, item, text, &hide)
            }
        }
    }
    fn show(&self, run_id: RunId, task: &Task, shown: Shown) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| append(tx, now, run_id, task.task_id, shown))
    }
}

pub(crate) fn append(
    tx: &mut store::Write<'_>,
    at: protocol::time::Timestamp,
    run_id: RunId,
    task_id: TaskId,
    shown: Shown,
) -> store::Result<()> {
    tx.emit(Event::TimelineAppended {
        task_id,
        item: TimelineItem {
            seq: 0,
            at,
            run_id: Some(run_id),
            item: TimelineBody::Shown { shown },
        },
    })?;
    Ok(())
}

/// Where a file an agent shows is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Folder {
    Project,
    /// The task's scratch folder.
    Scratch,
}

/// The folder a file an agent shows is in, and its path inside it,
/// `/`-separated. An agent may give it relative to the project, or as an
/// absolute path inside the project or the task's scratch folder.
pub(crate) fn shown_relative(
    project: &Project,
    scratch: &Path,
    raw: &str,
) -> Result<(Folder, String), ProtocolError> {
    let outside = || {
        error(
            ErrorCode::PathNotAllowed,
            "Only a file inside this project's folder or your scratch folder can be shown",
        )
    };
    let path = Path::new(raw);
    let resolved;
    let (folder, path) = if !path.is_absolute() {
        (Folder::Project, path)
    } else if let Ok(inside) = path.strip_prefix(&project.host_path) {
        (Folder::Project, inside)
    } else if let Ok(inside) = path.strip_prefix(scratch) {
        (Folder::Scratch, inside)
    } else {
        // Reached through another name for the folder, as a link to it.
        // Windows spells a resolved path with `\\?\`; the folder is kept
        // without it.
        resolved = ports::library::plain(std::fs::canonicalize(path).map_err(|_| outside())?);
        if let Ok(inside) = resolved.strip_prefix(&project.host_path) {
            (Folder::Project, inside)
        } else {
            let scratch =
                ports::library::plain(std::fs::canonicalize(scratch).map_err(|_| outside())?);
            (
                Folder::Scratch,
                resolved.strip_prefix(scratch).map_err(|_| outside())?,
            )
        }
    };
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_str().ok_or_else(outside)?),
            _ => return Err(outside()),
        }
    }
    Ok((folder, parts.join("/")))
}

/// An agent's page is one it serves on this computer. The person's own
/// previews may be anywhere on their network; an agent's may not.
pub(crate) fn loopback(raw: &str) -> Result<String, ProtocolError> {
    let url = crate::preview::validate(raw)?;
    let local = match url.host() {
        Some(url::Host::Domain(name)) => name == "localhost",
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    };
    if !local {
        return Err(error(
            ErrorCode::PathNotAllowed,
            "Give the page's address on this computer: http://127.0.0.1:PORT/ or http://localhost:PORT/",
        ));
    }
    Ok(url.to_string())
}

/// The command that starts a preview's server, which the person reads before
/// Ophio runs it: one line, with nothing hidden in it or reordering it.
pub(crate) fn start_command(
    start: Option<String>,
    hide: &impl Fn(&mut String),
) -> Result<Option<String>, ProtocolError> {
    let Some(start) = start.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if start.chars().count() > MAX_START
        || (start.chars()).any(|c| c.is_control() || ports::library::direction_control(c))
    {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Give start as one plain command line of at most 1000 characters",
        ));
    }
    let mut seen = start.clone();
    hide(&mut seen);
    if seen != start {
        return Err(error(
            ErrorCode::InvalidRequest,
            "That command holds a key or token this run was given, and the person sees it; give one that reads it from where the project keeps it",
        ));
    }
    Ok(Some(start))
}

/// Refuses an address hiding would change, or what it spells with its
/// escapes read over again: a page's server reads them, and so can the
/// person.
fn spelled_plainly(url: &str, hide: &impl Fn(&mut String)) -> Result<(), ProtocolError> {
    if url.len() > MAX_ADDRESS {
        return Err(error(
            ErrorCode::InvalidRequest,
            "That address is too long to show; give one of at most 8 KiB",
        ));
    }
    let changes = |text: &str| {
        let mut seen = text.to_owned();
        hide(&mut seen);
        seen != text
    };
    let mut spelled = url.to_owned();
    for _ in 0..MAX_READINGS {
        if changes(&spelled) || changes(&spelled.replace('+', " ")) {
            return Err(error(
                ErrorCode::InvalidRequest,
                "That address holds a key or token this run was given, and the person sees it; give the page's address without it",
            ));
        }
        let read = percent_decoded(&spelled);
        if read == spelled {
            return Ok(());
        }
        spelled = read;
    }
    Err(error(
        ErrorCode::InvalidRequest,
        "That address is escaped too many times over to check; give it with its escapes read",
    ))
}

/// The text with each `%XX` read as the byte it stands for.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |at: usize| bytes.get(at).and_then(|b| char::from(*b).to_digit(16));
    let mut read = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%'
            && let (Some(high), Some(low)) = (hex(at + 1), hex(at + 2))
        {
            read.push((high * 16 + low) as u8);
            at += 3;
        } else {
            read.push(byte);
            at += 1;
        }
    }
    String::from_utf8_lossy(&read).into_owned()
}

/// What the bytes are, read from the bytes. A name or an agent's word for
/// it decides nothing.
pub(crate) fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let at = |offset: usize, magic: &[u8]| bytes.get(offset..offset + magic.len()) == Some(magic);
    if at(0, b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if at(0, b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
        Some("image/gif")
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        Some("image/webp")
    } else if at(0, b"\x1a\x45\xdf\xa3") {
        // Matroska and WebM share a header; only WebM plays on a phone.
        bytes
            .get(..64)
            .unwrap_or(bytes)
            .windows(4)
            .any(|w| w == b"webm")
            .then_some("video/webm")
    } else if at(4, b"ftyp") {
        // HEIF and AVIF pictures are ISO media files too.
        match bytes.get(8..12)? {
            b"qt  " => Some("video/quicktime"),
            b"heic" | b"heix" | b"hevc" | b"heim" | b"heis" | b"mif1" | b"msf1" | b"avif"
            | b"avis" => None,
            _ => Some("video/mp4"),
        }
    } else {
        None
    }
}
