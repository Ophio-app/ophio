// SPDX-License-Identifier: Apache-2.0
//! A site that a program working in a project's folder serves on this
//! computer joins the project's previews by itself, so a dev server the
//! agent or the person started opens on the phone without being added. A
//! found preview is marked stopped when its server stops and removed once it
//! has stayed stopped a while. One the person removes stays away while that
//! server runs; started again, it is found again.
//!
//! A server runs while its socket is open, wherever its program works
//! meanwhile. What it answered, which preview it serves and whether the
//! person removed it is kept that long, through the companion starting again
//! in the same boot.
use crate::{CompanionHub, commands::emit_previews, internal, preview::PageAnswer};
use ports::servers::{Listening, LocalServers};
use protocol::{model::*, *};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError},
    time::Duration,
};

/// How often the computer's listening sockets are read.
pub(crate) const EVERY: Duration = Duration::from_secs(3);
/// How long a stopped server's preview is kept, in case it starts again
/// straight away, as a dev server restarting does. Control doesn't list a
/// found one meanwhile.
const KEPT_STOPPED: Duration = Duration::from_secs(30);
/// How long a preview someone added of a page on this computer is kept
/// while nothing has been seen serving it, so it can be added before its
/// server starts.
const KEPT_UNSERVED: Duration = Duration::from_secs(10 * 60);
/// How many times a server that hasn't answered is asked again.
pub(crate) const ASKS: u8 = 5;
/// How many servers are asked at most in one pass; the rest wait for the next.
pub(crate) const ASKED_AT_ONCE: usize = 8;
/// How many answers are kept at most, so with [`NAME_BYTES`] their names
/// are 256 KiB at most. Past it no new server is asked until one stops: one
/// let go would be asked again, and could show what has since stopped being
/// hidden.
pub(crate) const ANSWERS_KEPT: usize = 256;
/// The longest name kept, with what is hidden in it. A page whose name is
/// longer is named by where it is served.
pub(crate) const NAME_BYTES: usize = 1024;
/// Where what sockets answered is kept, by socket.
pub(crate) const ANSWERS: &str = "found_answer";
/// Where the servers serving previews are kept, by project and port.
pub(crate) const SERVING: &str = "found_serving";
/// Where the servers the person removed are kept, by project, port and
/// socket.
pub(crate) const REMOVALS: &str = "found_removal";
/// Where the page title each found preview is named from is kept, hidden,
/// by preview, while the preview is: its name may be cut short inside what
/// Ophio holds only later, so it is named from its title anew.
pub(crate) const TITLES: &str = "found_title";

pub(crate) struct Finder {
    servers: Arc<dyn LocalServers>,
    /// What each socket answered. Held through a pass, so passes never
    /// overlap.
    asked: tokio::sync::Mutex<HashMap<u64, Asked>>,
    /// The boot the sockets' numbers belong to, read on the first pass along
    /// with what was kept in it.
    boot: OnceLock<Option<String>>,
    removed: Mutex<Removed>,
    /// The previews someone added that were seen served since the companion
    /// started, so are kept only a while once their server stops.
    served: Mutex<HashSet<PreviewId>>,
}

/// What a socket answered when asked for its page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Asked {
    /// Its page, by its title with what Ophio held hidden in it.
    Page(Option<String>),
    Other,
    Silent(u8),
}

/// What an open socket answered, as kept.
#[derive(Serialize, Deserialize)]
struct KeptAnswer {
    boot: String,
    asked: Asked,
}

/// A server: the project and port it serves, and its socket.
type Server = (ProjectId, u16, u64);

/// A server serving a preview, or one whose found preview the person
/// removed, as kept. Its socket is written as text, as a number may be past
/// what is kept exactly.
#[derive(Serialize, Deserialize)]
struct KeptServer {
    boot: String,
    project_id: ProjectId,
    port: u16,
    socket: String,
}

impl KeptServer {
    fn new(boot: &str, (project_id, port, socket): Server) -> Self {
        Self {
            boot: boot.to_string(),
            project_id,
            port,
            socket: socket.to_string(),
        }
    }

    /// The server kept in `boot`.
    fn read(kept: serde_json::Value, boot: &str) -> Option<Server> {
        let kept: Self = serde_json::from_value(kept).ok()?;
        let socket = kept.socket.parse().ok()?;
        (kept.boot == boot).then_some((kept.project_id, kept.port, socket))
    }
}

#[derive(Default)]
struct Removed {
    /// The socket whose page each project's found preview of a port shows,
    /// while it is open; once it isn't, the one found serving that port in
    /// its place. A removal is the server's the preview shows, not another's
    /// found on its port meanwhile.
    serving: HashMap<(ProjectId, u16), u64>,
    /// Running servers whose found preview the person removed. Another
    /// server on the same port is another.
    by_person: HashSet<Server>,
    /// Removed since a pass began reading which servers run, so perhaps
    /// before [`Self::serving`] had what that read found: kept to that.
    since_read: BTreeSet<(ProjectId, u16)>,
}

impl Finder {
    pub(crate) fn new(servers: Arc<dyn LocalServers>) -> Self {
        Self {
            servers,
            asked: tokio::sync::Mutex::default(),
            boot: OnceLock::new(),
            removed: Mutex::default(),
            served: Mutex::default(),
        }
    }

    /// The person removed a found preview: leave its server alone while it
    /// runs. One that had stopped is found again when it starts.
    pub(crate) fn removed(
        &self,
        tx: &store::Write<'_>,
        preview: &PreviewEndpoint,
    ) -> store::Result<()> {
        let Some(port) = crate::preview::loopback_port(&preview.url) else {
            return Ok(());
        };
        let key = (preview.project_id, port);
        let server = {
            let mut removed = self.lock_removed();
            removed.since_read.insert(key);
            let server = (removed.serving.get(&key)).map(|&socket| (key.0, key.1, socket));
            if let Some(server) = server {
                removed.by_person.insert(server);
            }
            server
        };
        // Kept now, as the companion may stop before its next pass.
        match (server, self.boot.get()) {
            (Some(server), Some(Some(boot))) => {
                tx.put(REMOVALS, removal_id(server), &KeptServer::new(boot, server))
            }
            _ => Ok(()),
        }
    }

    /// What was kept in this boot, from before the companion started.
    /// What is held already is newer.
    fn load(
        &self,
        r: &store::Read<'_>,
        boot: &str,
        asked: &mut HashMap<u64, Asked>,
    ) -> store::Result<()> {
        for (id, kept) in r.entries::<serde_json::Value>(ANSWERS)? {
            if let (Ok(socket), Ok(kept)) = (id.parse(), serde_json::from_value::<KeptAnswer>(kept))
                && kept.boot == boot
                && asked.len() < ANSWERS_KEPT
            {
                asked.entry(socket).or_insert(kept.asked);
            }
        }
        let mut removed = self.lock_removed();
        for (_, kept) in r.entries::<serde_json::Value>(SERVING)? {
            if let Some((project_id, port, socket)) = KeptServer::read(kept, boot) {
                removed.serving.entry((project_id, port)).or_insert(socket);
            }
        }
        for (_, kept) in r.entries::<serde_json::Value>(REMOVALS)? {
            removed.by_person.extend(KeptServer::read(kept, boot));
        }
        Ok(())
    }

    fn lock_served(&self) -> MutexGuard<'_, HashSet<PreviewId>> {
        self.served.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_removed(&self) -> MutexGuard<'_, Removed> {
        self.removed.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Where a removed server is kept.
fn removal_id((project_id, port, socket): Server) -> String {
    format!("{project_id}:{port}:{socket}")
}

/// Brings what is kept of `kind` to `now`, by id: what is no longer held is
/// let go, and what is from another boot with it.
fn keep(
    tx: &store::Write<'_>,
    kind: &str,
    now: HashMap<String, serde_json::Value>,
) -> store::Result<()> {
    let kept: HashMap<String, serde_json::Value> = tx.read().entries(kind)?.into_iter().collect();
    for id in kept.keys().filter(|id| !now.contains_key(*id)) {
        tx.delete(kind, id)?;
    }
    for (id, value) in &now {
        if kept.get(id) != Some(value) {
            tx.put(kind, id, value)?;
        }
    }
    Ok(())
}

/// A page's title as its name, with what Ophio holds hidden in it, if it
/// isn't longer than any name.
fn named(mut title: String, hide: &impl Fn(&mut String)) -> Option<String> {
    hide(&mut title);
    (title.len() <= NAME_BYTES).then_some(title)
}

/// A found site's name: its page's title, or else where it is served.
fn label(url: &str, title: &Option<String>) -> String {
    title
        .as_deref()
        .map(crate::preview::short)
        .unwrap_or_else(|| {
            let host = url.trim_start_matches("http://");
            host.trim_end_matches('/').to_string()
        })
}

/// A project's site found this pass.
struct Found {
    project_id: ProjectId,
    port: u16,
    socket: u64,
    url: String,
}

impl CompanionHub {
    /// Read which servers run in projects' folders now and bring the
    /// projects' found previews up to date.
    pub(crate) async fn find_previews(&self) -> Result<(), ProtocolError> {
        let (Some(finder), Some(checker)) = (&self.inner.finder, &self.inner.preview) else {
            return Ok(());
        };
        let mut asked = finder.asked.lock().await;
        let projects: Vec<Project> = self.read(|r| r.list("project"))?;
        let servers = finder.servers.clone();
        // Before the first pass nothing is known to serve, so a preview
        // removed then is kept to what this one finds.
        let first = finder.boot.get().is_none();
        if !first {
            finder.lock_removed().since_read.clear();
        }
        // A check made since is newer than what this pass reads.
        let read_at = self.inner.clock.now();
        let (folders, listening, open, boot) = tokio::task::spawn_blocking(move || {
            // Programs report the folder they work in with links resolved. A
            // project's folder is kept with its links resolved when added, so
            // one that now leads elsewhere was replaced, and isn't looked in.
            // Home holds every project, so a server found there isn't Home's.
            let folders: Vec<(ProjectId, PathBuf)> = (projects.iter())
                .filter(|p| !p.home)
                .filter_map(|p| {
                    let folder = std::fs::canonicalize(&p.host_path).ok()?;
                    let folder = ports::library::plain(folder);
                    (folder == std::path::Path::new(&p.host_path)).then_some((p.project_id, folder))
                })
                .collect();
            let paths: Vec<PathBuf> = folders.iter().map(|(_, f)| f.clone()).collect();
            let listening = servers.listening(&paths);
            let open = servers.open();
            (folders, listening, open, first.then(|| servers.boot()))
        })
        .await
        .map_err(|_| internal("Reading which servers run stopped"))?;
        if let Some(boot) = boot {
            if let Some(boot) = &boot {
                self.read(|r| finder.load(r, boot, &mut asked))?;
            }
            let _ = finder.boot.set(boot);
        }

        // One site per project and port, though it may listen on both
        // IPv4 and IPv6.
        let mut found: BTreeMap<(ProjectId, u16), Found> = BTreeMap::new();
        for l in listening {
            if let Some(project_id) = owner(&folders, &l) {
                found.entry((project_id, l.port)).or_insert(Found {
                    project_id,
                    port: l.port,
                    socket: l.socket,
                    url: url_for(l.address, l.port),
                });
            }
        }
        // A server runs on while its program works outside every project
        // for a while: what it answered is kept, as asked again it could show
        // what has since stopped being hidden.
        let running: HashSet<u64> = (open.keys().copied())
            .chain(found.values().map(|f| f.socket))
            .collect();
        let listened: HashSet<u16> = (open.into_values())
            .chain(found.values().map(|f| f.port))
            .collect();
        asked.retain(|socket, _| running.contains(socket));
        // What serves each port is known from here, so a preview removed
        // while its server is asked keeps that server away.
        let by_person = {
            let mut removed = finder.lock_removed();
            removed.serving.retain(|_, socket| running.contains(socket));
            for (key, f) in &found {
                removed.serving.entry(*key).or_insert(f.socket);
            }
            let Removed {
                serving,
                by_person,
                since_read,
            } = &mut *removed;
            for key in std::mem::take(since_read) {
                if let Some(&socket) = serving.get(&key) {
                    by_person.insert((key.0, key.1, socket));
                }
            }
            // A removed server stays away until it is started again.
            by_person.retain(|(_, _, socket)| running.contains(socket));
            by_person.clone()
        };
        // Only a server that could become a preview is asked: not one the
        // person removed, nor one on the port of a preview someone added, nor
        // one in a project that has no room for another.
        let previews: Vec<PreviewEndpoint> = self.read(|r| r.list("preview"))?;
        let mut asking: Vec<(u8, &Found)> = (found.values())
            .filter_map(|f| {
                let tries = match asked.get(&f.socket) {
                    Some(Asked::Silent(tries)) if *tries < ASKS => *tries,
                    Some(_) => return None,
                    None => 0,
                };
                if by_person.contains(&(f.project_id, f.port, f.socket)) {
                    return None;
                }
                let theirs = previews.iter().filter(|p| p.project_id == f.project_id);
                let room = match (theirs.clone())
                    .find(|p| crate::preview::loopback_port(&p.url) == Some(f.port))
                {
                    Some(p) => p.automatic,
                    None => theirs.count() < crate::shown::MAX_PREVIEWS,
                };
                room.then_some((tries, f))
            })
            .collect();
        // New servers first; the rest are asked in later passes. A new one
        // is asked only while there is room to keep its answer.
        asking.sort_by_key(|(tries, f)| (*tries, f.port));
        let mut room = ANSWERS_KEPT.saturating_sub(asked.len());
        let asking = asking.into_iter().filter(|(_, f)| {
            let new = !asked.contains_key(&f.socket);
            let admitted = !new || room > 0;
            room -= usize::from(new && admitted);
            admitted
        });
        // A page names itself, and its name is hidden in as what an agent
        // says. It is kept hidden, with what Ophio held as it was asked: the
        // run that started it may end meanwhile, and with it what is hidden.
        let hide = self.found_masking();
        // Those asked are asked at once, so one that never answers holds up
        // no other.
        let mut answers = tokio::task::JoinSet::new();
        for (tries, f) in asking.take(ASKED_AT_ONCE) {
            let Ok(url) = url::Url::parse(&f.url) else {
                continue;
            };
            let (checker, socket) = (checker.clone(), f.socket);
            answers.spawn(async move {
                let wait = crate::preview::PAGE_WAIT + Duration::from_secs(1);
                let answer = tokio::time::timeout(wait, checker.page(&url)).await;
                (socket, tries, answer)
            });
        }
        while let Some(joined) = answers.join_next().await {
            let Ok((socket, tries, answer)) = joined else {
                continue;
            };
            let asked_now = match answer {
                Ok(Ok(PageAnswer::Page(title))) => Asked::Page(title.and_then(|t| named(t, &hide))),
                Ok(Ok(PageAnswer::Other)) => Asked::Other,
                _ => Asked::Silent(tries + 1),
            };
            asked.insert(socket, asked_now);
        }
        // And hidden again where it is kept, with what Ophio holds now: what
        // is hidden once stays hidden, whatever Ophio holds later.
        let hide = self.found_masking();
        for asked in asked.values_mut() {
            if let Asked::Page(title) = asked {
                *title = title.take().and_then(|t| named(t, &hide));
            }
        }
        let pages: Vec<(&Found, Option<String>)> = (found.values())
            .filter_map(|f| match asked.get(&f.socket) {
                Some(Asked::Page(title)) => Some((f, title.clone())),
                _ => None,
            })
            .collect();

        let now = self.inner.clock.now();
        self.write(|tx| {
            let mut removed = finder.lock_removed();
            let previews: Vec<PreviewEndpoint> = tx.read().list("preview")?;
            // A project may have been removed while its servers were asked.
            let projects: BTreeSet<ProjectId> = (tx.read().list::<Project>("project")?.iter())
                .map(|p| p.project_id)
                .collect();
            let mut counts: HashMap<ProjectId, usize> = HashMap::new();
            for p in &previews {
                *counts.entry(p.project_id).or_default() += 1;
            }
            let mut changed = BTreeSet::new();
            let mut serving = BTreeSet::new();
            let mut titles: HashMap<String, String> = (tx.read().entries(TITLES)?.into_iter())
                .filter_map(|(id, title)| Some((id, serde_json::from_value(title).ok()?)))
                .collect();
            let mut named_from = |id: PreviewId, title: &Option<String>| match title {
                Some(title) => titles.insert(id.to_string(), title.clone()),
                None => titles.remove(&id.to_string()),
            };
            for (f, title) in &pages {
                let key = (f.project_id, f.port);
                if removed
                    .by_person
                    .contains(&(f.project_id, f.port, f.socket))
                    || !projects.contains(&f.project_id)
                {
                    continue;
                }
                serving.insert(key);
                let known = (previews.iter()).find(|p| {
                    p.project_id == f.project_id
                        && crate::preview::loopback_port(&p.url) == Some(f.port)
                });
                match known {
                    // The person's or the agent's own is theirs to check.
                    Some(p) if !p.automatic => {}
                    // A server started again may listen at another address
                    // or name its page anew.
                    Some(p) => {
                        removed.serving.insert(key, f.socket);
                        named_from(p.preview_id, title);
                        let now_found = PreviewEndpoint {
                            label: label(&f.url, title),
                            url: f.url.clone(),
                            reachable: Some(true),
                            checked_at: Some(now),
                            ..p.clone()
                        };
                        if (&now_found.label, &now_found.url, now_found.reachable)
                            != (&p.label, &p.url, p.reachable)
                        {
                            tx.put("preview", p.preview_id, &now_found)?;
                            changed.insert(p.project_id);
                        }
                    }
                    None => {
                        let count = counts.entry(f.project_id).or_default();
                        if *count >= crate::shown::MAX_PREVIEWS {
                            continue;
                        }
                        *count += 1;
                        removed.serving.insert(key, f.socket);
                        let p = PreviewEndpoint {
                            preview_id: PreviewId::new(),
                            project_id: f.project_id,
                            label: label(&f.url, title),
                            url: f.url.clone(),
                            reachable: Some(true),
                            checked_at: Some(now),
                            automatic: true,
                            start: None,
                        };
                        named_from(p.preview_id, title);
                        tx.put("preview", p.preview_id, &p)?;
                        changed.insert(p.project_id);
                    }
                }
            }
            for p in previews.iter().filter(|p| p.automatic) {
                if !projects.contains(&p.project_id) {
                    tx.delete("preview", p.preview_id)?;
                    continue;
                }
                let port = crate::preview::loopback_port(&p.url);
                if port.is_some_and(|port| serving.contains(&(p.project_id, port))) {
                    continue;
                }
                // Not found now, it is named with what Ophio holds now all
                // the same, from the page's title it was named from, as a
                // page is: by its address once that is longer than any name.
                let id = p.preview_id.to_string();
                let title = (titles.remove(&id)).and_then(|title| named(title, &hide));
                let named = PreviewEndpoint {
                    label: label(&p.url, &title),
                    ..p.clone()
                };
                titles.extend(title.map(|title| (id, title)));
                match (p.reachable, p.checked_at) {
                    (Some(false), Some(at))
                        if now.0.saturating_sub(at.0) < KEPT_STOPPED.as_millis() as i64 =>
                    {
                        if named.label == p.label {
                            continue;
                        }
                        tx.put("preview", p.preview_id, &named)?;
                    }
                    (Some(false), _) => tx.delete("preview", p.preview_id)?,
                    _ => {
                        let p = PreviewEndpoint {
                            reachable: Some(false),
                            checked_at: Some(now),
                            ..named
                        };
                        tx.put("preview", p.preview_id, &p)?;
                    }
                }
                changed.insert(p.project_id);
            }
            // A preview the person or an agent added of a page on this
            // computer follows its server too: one seen served goes a while
            // after its server stops, as a found one does, and one never
            // seen served after longer.
            let mut served = finder.lock_served();
            for p in previews.iter().filter(|p| !p.automatic) {
                let Some(port) = crate::preview::loopback_port(&p.url) else {
                    continue;
                };
                if !projects.contains(&p.project_id)
                    || p.checked_at.is_some_and(|at| at.0 > read_at.0)
                {
                    continue;
                }
                let reachable = listened.contains(&port);
                if reachable || p.reachable == Some(true) {
                    served.insert(p.preview_id);
                }
                match (p.reachable, p.checked_at) {
                    (Some(true), _) if reachable => continue,
                    (Some(false), Some(at)) if !reachable => {
                        let kept = match served.contains(&p.preview_id) {
                            true => KEPT_STOPPED,
                            false => KEPT_UNSERVED,
                        };
                        // One its server can be started for is kept until
                        // the person removes it.
                        if p.start.is_some() || now.0.saturating_sub(at.0) < kept.as_millis() as i64
                        {
                            continue;
                        }
                        tx.delete("preview", p.preview_id)?;
                    }
                    // Started or stopped since, or not known before.
                    _ => {
                        let p = PreviewEndpoint {
                            reachable: Some(reachable),
                            checked_at: Some(now),
                            ..p.clone()
                        };
                        tx.put("preview", p.preview_id, &p)?;
                    }
                }
                changed.insert(p.project_id);
            }
            served.retain(|id| previews.iter().any(|p| p.preview_id == *id));
            drop(served);
            for project_id in changed {
                emit_previews(tx, project_id)?;
            }
            // Kept while the sockets are open, for a companion started again
            // in this boot, so none is asked more than it would have been.
            // Nothing is kept where the boot isn't known.
            let boot = finder.boot.get().cloned().flatten();
            let boot = boot.as_deref();
            let answers = (asked.iter()).filter_map(|(socket, asked)| {
                let kept = KeptAnswer {
                    boot: boot?.to_string(),
                    asked: asked.clone(),
                };
                Some((socket.to_string(), serde_json::to_value(kept).ok()?))
            });
            keep(tx, ANSWERS, answers.collect())?;
            // What serves each preview too, so one removed after the
            // companion starts again keeps its server away though it works
            // elsewhere meanwhile.
            let kept: Vec<PreviewEndpoint> = tx.read().list("preview")?;
            let previewed: BTreeSet<(ProjectId, u16)> = (kept.iter())
                .filter_map(|p| Some((p.project_id, crate::preview::loopback_port(&p.url)?)))
                .collect();
            let servers = (removed.serving.iter())
                .filter(|(key, _)| previewed.contains(key))
                .filter_map(|(&(project_id, port), &socket)| {
                    let kept = KeptServer::new(boot?, (project_id, port, socket));
                    Some((
                        format!("{project_id}:{port}"),
                        serde_json::to_value(kept).ok()?,
                    ))
                });
            keep(tx, SERVING, servers.collect())?;
            let removals = (removed.by_person.iter()).filter_map(|&server| {
                let kept = KeptServer::new(boot?, server);
                Some((removal_id(server), serde_json::to_value(kept).ok()?))
            });
            keep(tx, REMOVALS, removals.collect())?;
            // And the titles of the found previews kept, through restarts.
            let titles = (kept.iter().filter(|p| p.automatic)).filter_map(|p| {
                let id = p.preview_id.to_string();
                let title = titles.remove(&id)?;
                Some((id, serde_json::Value::String(title)))
            });
            keep(tx, TITLES, titles.collect())?;
            Ok(())
        })
    }
}

/// The project a program works in: the deepest whose folder holds its own.
fn owner(folders: &[(ProjectId, PathBuf)], l: &Listening) -> Option<ProjectId> {
    (folders.iter())
        .filter(|(_, folder)| l.dir.starts_with(folder))
        .max_by_key(|(_, folder)| folder.components().count())
        .map(|(project_id, _)| *project_id)
}

/// The address the page is opened at here: the one the server listens on,
/// or for a server listening on every address, loopback of the same kind.
/// Never `localhost`, which could reach another server on the same port.
fn url_for(address: IpAddr, port: u16) -> String {
    let address = match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 => v4,
    };
    match address {
        IpAddr::V4(ip) if ip.is_unspecified() => format!("http://{}:{port}/", Ipv4Addr::LOCALHOST),
        IpAddr::V4(ip) => format!("http://{ip}:{port}/"),
        IpAddr::V6(ip) if ip.is_unspecified() => {
            format!("http://[{}]:{port}/", Ipv6Addr::LOCALHOST)
        }
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_server_where_it_listens() {
        let url = |a: &str| url_for(a.parse().unwrap(), 5173);
        for (listening, opened) in [
            ("0.0.0.0", "http://127.0.0.1:5173/"),
            ("127.0.0.1", "http://127.0.0.1:5173/"),
            ("::ffff:127.0.0.1", "http://127.0.0.1:5173/"),
            ("::ffff:0.0.0.0", "http://127.0.0.1:5173/"),
            ("127.0.0.2", "http://127.0.0.2:5173/"),
            ("::ffff:127.0.0.2", "http://127.0.0.2:5173/"),
            ("::", "http://[::1]:5173/"),
            ("::1", "http://[::1]:5173/"),
        ] {
            assert_eq!(url(listening), opened, "{listening}");
        }
    }
}
