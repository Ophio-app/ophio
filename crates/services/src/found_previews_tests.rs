// SPDX-License-Identifier: Apache-2.0
//! A site served from a project's folder joins its previews by itself.
use super::*;
use crate::found_previews::{
    ANSWERS, ANSWERS_KEPT, ASKED_AT_ONCE, ASKS, NAME_BYTES, REMOVALS, SERVING, TITLES,
};
use crate::preview::{PageAnswer, PreviewChecker};
use ports::Clock;
use ports::servers::{Listening, LocalServers};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
    sync::Mutex,
};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Something done once, while the next server is read or asked.
#[derive(Default)]
pub(super) struct Meanwhile(Mutex<Option<Box<dyn FnOnce() + Send>>>);
impl std::fmt::Debug for Meanwhile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Meanwhile").finish_non_exhaustive()
    }
}
impl Meanwhile {
    pub(super) fn set(&self, done: impl FnOnce() + Send + 'static) {
        *self.0.lock().unwrap() = Some(Box::new(done));
    }
    fn run(&self) {
        let done = self.0.lock().unwrap().take();
        if let Some(done) = done {
            done();
        }
    }
}

/// The servers running, as the computer would list them.
#[derive(Debug)]
pub(super) struct Servers {
    running: Mutex<Vec<Listening>>,
    /// Done while the computer is next read.
    meanwhile: Meanwhile,
    boot: Mutex<Option<String>>,
}
impl Default for Servers {
    fn default() -> Self {
        Self {
            running: Mutex::default(),
            meanwhile: Meanwhile::default(),
            boot: Mutex::new(Some("first boot".into())),
        }
    }
}
impl Servers {
    pub(super) fn serve(&self, address: IpAddr, port: u16, dir: &Path, socket: u64) {
        self.running.lock().unwrap().push(Listening {
            address,
            port,
            dir: dir.to_path_buf(),
            socket,
        });
    }
    pub(super) fn stop(&self, port: u16) {
        self.running.lock().unwrap().retain(|l| l.port != port);
    }
    /// The server listening on `socket` stops.
    fn closes(&self, socket: u64) {
        self.running.lock().unwrap().retain(|l| l.socket != socket);
    }
    /// The program serving `port` goes to work in `dir`, listening still.
    pub(super) fn moves(&self, port: u16, dir: &Path) {
        for l in self.running.lock().unwrap().iter_mut() {
            if l.port == port {
                l.dir = dir.to_path_buf();
            }
        }
    }
}
impl LocalServers for Servers {
    fn listening(&self, folders: &[PathBuf]) -> Vec<Listening> {
        self.meanwhile.run();
        let servers = self.running.lock().unwrap();
        (servers.iter())
            .filter(|l| folders.iter().any(|f| l.dir.starts_with(f)))
            .cloned()
            .collect()
    }
    fn open(&self) -> HashMap<u64, u16> {
        (self.running.lock().unwrap().iter())
            .map(|l| (l.socket, l.port))
            .collect()
    }
    fn boot(&self) -> Option<String> {
        self.boot.lock().unwrap().clone()
    }
}

/// What each port answers when asked for its page, and every ask.
#[derive(Debug, Default)]
pub(super) struct Pages {
    answers: Mutex<HashMap<u16, PageAnswer>>,
    asked: Mutex<Vec<String>>,
    /// Done while the next server is asked.
    pub(super) meanwhile: Meanwhile,
}
impl Pages {
    pub(super) fn answer(&self, port: u16, answer: PageAnswer) {
        self.answers.lock().unwrap().insert(port, answer);
    }
    pub(super) fn asks(&self, url: &str) -> usize {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|u| *u == url)
            .count()
    }
}
#[async_trait::async_trait]
impl PreviewChecker for Pages {
    async fn check(&self, _: &url::Url) -> Result<bool, ProtocolError> {
        Ok(true)
    }
    async fn page(&self, url: &url::Url) -> Result<PageAnswer, ProtocolError> {
        self.asked.lock().unwrap().push(url.to_string());
        self.meanwhile.run();
        let answers = self.answers.lock().unwrap();
        Ok(answers
            .get(&url.port().unwrap())
            .cloned()
            .unwrap_or(PageAnswer::Silent))
    }
}

struct Finding {
    f: Fixture,
    servers: Arc<Servers>,
    pages: Arc<Pages>,
    /// The project's folder as programs working in it report it.
    dir: PathBuf,
    project_id: ProjectId,
}
impl Finding {
    async fn new() -> Self {
        let servers = Arc::new(Servers::default());
        let pages = Arc::new(Pages::default());
        let f = Fixture::with(|b| {
            b.local_servers(servers.clone())
                .preview_checker(pages.clone())
        })
        .await;
        let project_id = f.project().await;
        let dir = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
        Self {
            f,
            servers,
            pages,
            dir,
            project_id,
        }
    }
    async fn pass(&self) {
        self.f.hub.find_previews().await.unwrap();
    }
    /// The companion stopped and started again.
    async fn restarted(self) -> Self {
        let Self {
            f,
            servers,
            pages,
            dir,
            project_id,
        } = self;
        let f = {
            let (servers, pages) = (servers.clone(), pages.clone());
            f.reopened(|b| b.local_servers(servers).preview_checker(pages))
                .await
        };
        Self {
            f,
            servers,
            pages,
            dir,
            project_id,
        }
    }
    /// The boots of what is kept of `kind`.
    fn kept(&self, kind: &str) -> Vec<String> {
        let kept: Vec<serde_json::Value> = self.f.hub.read(|r| r.list(kind)).unwrap();
        (kept.iter())
            .map(|k| k["boot"].as_str().unwrap().to_string())
            .collect()
    }
    fn previews(&self) -> Vec<PreviewEndpoint> {
        let mut previews: Vec<PreviewEndpoint> = self.f.hub.read(|r| r.list("preview")).unwrap();
        previews.sort_by(|a, b| a.url.cmp(&b.url));
        previews
    }
    fn announced(&self) -> usize {
        let journal = self.f.hub.read(|r| r.journal(0)).unwrap();
        (journal.events.iter())
            .filter(|e| matches!(e.event, Event::PreviewsChanged { .. }))
            .count()
    }
    /// Removes the found preview as the person does, from anywhere.
    fn removing(&self) -> impl FnOnce() + Send + 'static {
        let (hub, preview) = (self.f.hub.clone(), self.previews()[0].clone());
        move || {
            hub.write(|tx| {
                tx.delete("preview", preview.preview_id)?;
                hub.inner.finder.as_ref().unwrap().removed(tx, &preview)
            })
            .unwrap();
        }
    }
    async fn remove(&self, preview_id: PreviewId) {
        let receipt = self
            .f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::RemovePreview { preview_id }),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
}

#[tokio::test]
async fn a_site_served_from_a_project_folder_joins_its_previews() {
    let plain = Fixture::new().await;
    let features = plain.hub.host_capabilities().await.features;
    assert!(!features.contains(&"found_previews_v1".to_string()));
    plain.close().await;

    let s = Finding::new().await;
    let features = s.f.hub.host_capabilities().await.features;
    assert!(features.contains(&"found_previews_v1".to_string()));
    let web = s.dir.join("web");
    // One server listening on both IPv4 and IPv6 is one site.
    s.servers.serve(LOOPBACK, 5173, &web, 1);
    s.servers
        .serve(IpAddr::V6(Ipv6Addr::LOCALHOST), 5173, &web, 2);
    s.servers
        .serve(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8080, &s.dir, 3);
    s.servers
        .serve("127.0.0.2".parse().unwrap(), 4000, &s.dir, 4);
    s.servers.serve(LOOPBACK, 9229, &s.dir, 5);
    // Not this project's.
    s.servers.serve(LOOPBACK, 3000, Path::new("/elsewhere"), 6);
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.pages.answer(8080, PageAnswer::Page(None));
    s.pages.answer(4000, PageAnswer::Page(None));
    s.pages.answer(9229, PageAnswer::Other);
    s.pages.answer(3000, PageAnswer::Page(None));
    s.pass().await;

    let now = s.f.clock.now();
    let found: Vec<_> = (s.previews().into_iter())
        .map(|p| {
            assert!(p.automatic);
            assert_eq!(p.project_id, s.project_id);
            assert_eq!((p.reachable, p.checked_at), (Some(true), Some(now)));
            (p.label, p.url)
        })
        .collect();
    assert_eq!(
        found,
        [
            ("Shop", "http://127.0.0.1:5173/"),
            ("127.0.0.2:4000", "http://127.0.0.2:4000/"),
            ("[::1]:8080", "http://[::1]:8080/"),
        ]
        .map(|(label, url): (&str, &str)| (label.to_string(), url.to_string()))
    );
    assert_eq!(s.pages.asks("http://127.0.0.1:3000/"), 0);
    assert_eq!(s.announced(), 1);
    // Each server is asked once.
    let asked = s.pages.asked.lock().unwrap().len();
    s.pass().await;
    assert_eq!(s.pages.asked.lock().unwrap().len(), asked);
    assert_eq!(s.announced(), 1);
    s.f.close().await;
}

#[tokio::test]
async fn a_site_that_isnt_answering_yet_is_asked_again_a_few_times() {
    let s = Finding::new().await;
    s.servers.serve(LOOPBACK, 3000, &s.dir, 1);
    for _ in 0..8 {
        s.pass().await;
    }
    assert_eq!(s.pages.asks("http://127.0.0.1:3000/"), 5);
    s.pages.answer(3000, PageAnswer::Page(None));
    s.pass().await;
    assert!(s.previews().is_empty());
    // Started again, it is asked again.
    s.servers.stop(3000);
    s.servers.serve(LOOPBACK, 3000, &s.dir, 2);
    s.pass().await;
    assert_eq!(s.previews().len(), 1);
    // What a stopped server answered isn't kept for another that gets its
    // socket's number.
    s.pages.answer(9229, PageAnswer::Other);
    s.servers.serve(LOOPBACK, 9229, &s.dir, 3);
    s.pass().await;
    s.servers.stop(9229);
    s.pass().await;
    s.pages.answer(9229, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 9229, &s.dir, 3);
    s.pass().await;
    assert_eq!(s.previews().len(), 2);
    s.f.close().await;
}

#[tokio::test]
async fn a_stopped_site_is_kept_a_while_in_case_it_starts_again() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    let id = s.previews()[0].preview_id;
    s.servers.stop(5173);
    s.pass().await;
    let stopped = s.f.clock.now();
    assert_eq!(
        (s.previews()[0].reachable, s.previews()[0].checked_at),
        (Some(false), Some(stopped))
    );
    s.f.clock.advance(Duration::from_secs(20));
    s.pass().await;
    assert_eq!(s.previews()[0].checked_at, Some(stopped));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 2);
    s.pass().await;
    let again = &s.previews()[0];
    assert_eq!((again.preview_id, again.reachable), (id, Some(true)));
    s.servers.stop(5173);
    s.pass().await;
    s.f.clock.advance(Duration::from_secs(30));
    s.pass().await;
    assert!(s.previews().is_empty());
    s.f.close().await;
}

#[tokio::test]
async fn a_site_started_again_at_another_address_is_opened_there() {
    let s = Finding::new().await;
    let ipv6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    let id = s.previews()[0].preview_id;
    let now = |s: &Finding| {
        let p = &s.previews()[0];
        assert_eq!(p.preview_id, id);
        (p.label.clone(), p.url.clone(), p.reachable)
    };
    // Restarted between passes, at IPv6 and with another title.
    s.servers.stop(5173);
    s.servers.serve(ipv6, 5173, &s.dir, 2);
    s.pages.answer(5173, PageAnswer::Page(None));
    s.pass().await;
    assert_eq!(
        now(&s),
        ("[::1]:5173".into(), "http://[::1]:5173/".into(), Some(true))
    );
    assert_eq!(s.announced(), 2);
    // Stopped for a pass, named as it was last found, then back at IPv4.
    s.servers.stop(5173);
    s.pass().await;
    assert_eq!(
        now(&s),
        (
            "[::1]:5173".into(),
            "http://[::1]:5173/".into(),
            Some(false)
        )
    );
    s.servers.serve(LOOPBACK, 5173, &s.dir, 3);
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.pass().await;
    assert_eq!(
        now(&s),
        ("Shop".into(), "http://127.0.0.1:5173/".into(), Some(true))
    );
    // Nothing new, nothing announced.
    let announced = s.announced();
    s.pass().await;
    assert_eq!(s.announced(), announced);
    s.f.close().await;
}

#[tokio::test]
async fn the_persons_own_preview_of_a_port_is_theirs_and_goes_once_its_server_stops() {
    let s = Finding::new().await;
    let mine = register(&s, "http://127.0.0.1:5173/app").await;
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    // Nor is its server asked, but it is known to serve.
    assert!(s.pages.asked.lock().unwrap().is_empty());
    let own = |s: &Finding| {
        let p = s.previews().into_iter().find(|p| p.preview_id == mine)?;
        Some((p.label, p.automatic, p.reachable, p.checked_at))
    };
    let started = s.f.clock.now();
    assert_eq!(
        own(&s),
        Some(("Mine".into(), false, Some(true), Some(started)))
    );
    assert_eq!(s.previews().len(), 1);
    // Stopped, it is kept a while in case it starts again.
    s.f.clock.advance(Duration::from_secs(5));
    s.servers.stop(5173);
    s.pass().await;
    let stopped = s.f.clock.now();
    assert_eq!(
        own(&s),
        Some(("Mine".into(), false, Some(false), Some(stopped)))
    );
    s.f.clock.advance(Duration::from_secs(20));
    s.pass().await;
    assert_eq!(own(&s).unwrap().3, Some(stopped));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 2);
    s.pass().await;
    assert_eq!(own(&s).unwrap().2, Some(true));
    // And then it goes.
    s.servers.stop(5173);
    s.pass().await;
    s.f.clock.advance(Duration::from_secs(30));
    s.pass().await;
    assert!(s.previews().is_empty());
    s.f.close().await;
}

async fn register(s: &Finding, url: &str) -> PreviewId {
    let receipt =
        s.f.hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::RegisterPreview {
                    project_id: s.project_id,
                    label: "Mine".into(),
                    url: url.into(),
                }),
            )
            .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    s.previews()
        .into_iter()
        .find(|p| p.url == url)
        .unwrap()
        .preview_id
}

#[tokio::test]
async fn a_preview_added_before_its_server_starts_is_kept_longer() {
    let s = Finding::new().await;
    let mine = register(&s, "http://localhost:4178/").await;
    // On another computer, and so not followed.
    register(&s, "http://192.168.1.5:4178/").await;
    s.pass().await;
    let added = s.f.clock.now();
    let own = |s: &Finding| {
        let p = s.previews().into_iter().find(|p| p.preview_id == mine)?;
        Some((p.reachable, p.checked_at))
    };
    assert_eq!(own(&s), Some((Some(false), Some(added))));
    s.f.clock.advance(Duration::from_secs(9 * 60));
    s.pass().await;
    assert_eq!(own(&s), Some((Some(false), Some(added))));
    s.f.clock.advance(Duration::from_secs(60));
    s.pass().await;
    assert_eq!(own(&s), None);
    assert_eq!(s.previews().len(), 1);
    s.f.close().await;
}

#[tokio::test]
async fn a_preview_its_server_can_be_started_for_stays_once_the_server_stops() {
    let s = Finding::new().await;
    let mine = register(&s, "http://127.0.0.1:5179/").await;
    // As an agent shows it, with the command that starts its server.
    s.f.hub
        .write(|tx| {
            let mut p: PreviewEndpoint = tx.read().require("preview", mine)?;
            p.start = Some("npx vite --port 5179".into());
            tx.put("preview", mine, &p)?;
            Ok(())
        })
        .unwrap();
    s.servers.serve(LOOPBACK, 5179, &s.dir, 1);
    s.pass().await;
    s.servers.stop(5179);
    s.pass().await;
    s.f.clock.advance(Duration::from_secs(60 * 60));
    s.pass().await;
    let kept = (s.previews().into_iter()).find(|p| p.preview_id == mine);
    assert_eq!(kept.map(|p| p.reachable), Some(Some(false)));
    s.remove(mine).await;
    assert!(s.previews().is_empty());
    s.f.close().await;
}

#[tokio::test]
async fn a_check_made_while_servers_are_read_is_newer() {
    let s = Finding::new().await;
    let mine = register(&s, "http://127.0.0.1:4178/").await;
    let checked = s.f.clock.now();
    let hub = s.f.hub.clone();
    let clock = s.f.clock.clone();
    // The person checks it as the computer is read, once it has started.
    s.servers.meanwhile.set(move || {
        clock.advance(Duration::from_secs(1));
        hub.write(|tx| {
            let mut p: PreviewEndpoint = tx.read().require("preview", mine)?;
            p.reachable = Some(true);
            p.checked_at = Some(clock.now());
            tx.put("preview", mine, &p)
        })
        .unwrap();
    });
    s.pass().await;
    let p = &s.previews()[0];
    assert_eq!(p.reachable, Some(true));
    assert!(p.checked_at.unwrap().0 > checked.0);
    s.f.close().await;
}

#[tokio::test]
async fn a_site_the_person_removes_stays_away_until_its_server_starts_again() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    let titles = || s.f.hub.read(|r| r.list::<String>(TITLES)).unwrap();
    assert_eq!(titles(), ["Shop"]);
    s.remove(s.previews()[0].preview_id).await;
    s.pass().await;
    s.pass().await;
    assert!(s.previews().is_empty());
    // What it was named from goes with it.
    assert!(titles().is_empty());
    s.servers.stop(5173);
    s.pass().await;
    assert!(s.kept(REMOVALS).is_empty());
    s.servers.serve(LOOPBACK, 5173, &s.dir, 2);
    s.pass().await;
    assert_eq!(s.previews().len(), 1);
    // Restarted between two passes, it is found again too.
    s.remove(s.previews()[0].preview_id).await;
    s.pass().await;
    assert!(s.previews().is_empty());
    s.servers.stop(5173);
    s.servers.serve(LOOPBACK, 5173, &s.dir, 4);
    s.pass().await;
    assert_eq!(s.previews().len(), 1);
    // One removed after it stopped is found when it starts.
    s.servers.stop(5173);
    s.pass().await;
    s.remove(s.previews()[0].preview_id).await;
    s.servers.serve(LOOPBACK, 5173, &s.dir, 3);
    s.pass().await;
    assert_eq!(s.previews().len(), 1);
    s.f.close().await;
}

#[tokio::test]
async fn a_site_belongs_to_the_deepest_project_and_a_full_list_takes_no_more() {
    let s = Finding::new().await;
    let inner_dir = s.f.project_dir().join("site");
    std::fs::create_dir_all(&inner_dir).unwrap();
    let receipt =
        s.f.hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AddProject {
                    host_path: inner_dir.to_string_lossy().into_owned(),
                    name: None,
                }),
            )
            .await;
    let Some(CommandResult::ProjectAdded { project_id: inner }) = receipt.result else {
        panic!("project expected: {receipt:?}")
    };
    s.pages.answer(5173, PageAnswer::Page(None));
    s.pages.answer(8080, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 5173, &s.dir.join("site/app"), 1);
    s.servers.serve(LOOPBACK, 8080, &s.dir.join("api"), 2);
    s.pass().await;
    let owners: Vec<_> = (s.previews().into_iter())
        .map(|p| (p.url, p.project_id))
        .collect();
    assert_eq!(
        owners,
        [
            ("http://127.0.0.1:5173/".to_string(), inner),
            ("http://127.0.0.1:8080/".to_string(), s.project_id),
        ]
    );

    // The outer project's list fills up.
    s.f.hub
        .write(|tx| {
            for n in 0..crate::shown::MAX_PREVIEWS - 1 {
                let p = PreviewEndpoint {
                    preview_id: PreviewId::new(),
                    project_id: s.project_id,
                    label: format!("Kept {n}"),
                    url: format!("http://192.168.1.2:{}/", 9000 + n),
                    reachable: None,
                    checked_at: None,
                    automatic: false,
                    start: None,
                };
                tx.put("preview", p.preview_id, &p)?;
            }
            Ok(())
        })
        .unwrap();
    s.pages.answer(4000, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 4000, &s.dir, 3);
    s.pass().await;
    let outer = (s.previews().into_iter())
        .filter(|p| p.project_id == s.project_id)
        .count();
    assert_eq!(outer, crate::shown::MAX_PREVIEWS);
    assert!(!s.previews().iter().any(|p| p.url.contains(":4000")));
    // With no room, it isn't even asked.
    assert_eq!(s.pages.asks("http://127.0.0.1:4000/"), 0);
    s.f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_replaced_project_folder_is_not_looked_in() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(None));
    // The folder is moved away and a link to it put in its place.
    let moved = s.dir.with_extension("moved");
    std::fs::rename(&s.dir, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &s.dir).unwrap();
    s.servers.serve(LOOPBACK, 5173, &moved, 1);
    s.pass().await;
    assert!(s.previews().is_empty());
    assert!(s.pages.asked.lock().unwrap().is_empty());
    // Put back, it is looked in again.
    std::fs::remove_file(&s.dir).unwrap();
    std::fs::rename(&moved, &s.dir).unwrap();
    s.servers.stop(5173);
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    assert_eq!(s.previews().len(), 1);
    s.f.close().await;
}

#[tokio::test]
async fn only_a_few_servers_are_asked_at_once_and_new_ones_first() {
    let s = Finding::new().await;
    for n in 0..20 {
        s.servers
            .serve(LOOPBACK, 7000 + n, &s.dir, u64::from(n) + 1);
    }
    s.pass().await;
    assert_eq!(s.pages.asked.lock().unwrap().len(), ASKED_AT_ONCE);
    s.pass().await;
    assert_eq!(s.pages.asked.lock().unwrap().len(), 2 * ASKED_AT_ONCE);
    // One that starts now is asked before those asked already.
    s.pages.answer(9000, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 9000, &s.dir, 100);
    s.pass().await;
    assert_eq!(s.pages.asks("http://127.0.0.1:9000/"), 1);
    assert_eq!(s.previews().len(), 1);
    // Those never asked come before any asked again.
    assert_eq!(s.pages.asked.lock().unwrap().len(), 3 * ASKED_AT_ONCE);
    for n in 0..20 {
        let asks = s.pages.asks(&format!("http://127.0.0.1:{}/", 7000 + n));
        assert!((1..=2).contains(&asks), "{n}: {asks}");
    }
    s.f.close().await;
}

#[tokio::test]
async fn a_project_removed_while_its_servers_are_asked_gets_none() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(None));
    s.pages.answer(8080, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    assert_eq!((s.previews().len(), s.announced()), (1, 1));
    let (hub, project_id) = (s.f.hub.clone(), s.project_id);
    s.pages.meanwhile.set(move || {
        hub.write(|tx| tx.delete("project", project_id)).unwrap();
    });
    s.servers.serve(LOOPBACK, 8080, &s.dir, 2);
    s.pass().await;
    assert_eq!(s.pages.asks("http://127.0.0.1:8080/"), 1);
    // Neither the new one nor the one it had is kept.
    assert!(s.previews().is_empty());
    assert_eq!(s.announced(), 1);
    s.f.close().await;
}

#[tokio::test]
async fn a_server_the_person_removed_is_not_asked_again() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    // Restarted, it doesn't answer yet when the person removes its preview.
    s.servers.stop(5173);
    s.pages.answers.lock().unwrap().remove(&5173);
    s.servers.serve(LOOPBACK, 5173, &s.dir, 2);
    s.pass().await;
    assert_eq!(s.pages.asks("http://127.0.0.1:5173/"), 2);
    s.remove(s.previews()[0].preview_id).await;
    s.pass().await;
    s.pass().await;
    assert_eq!(s.pages.asks("http://127.0.0.1:5173/"), 2);
    assert!(s.previews().is_empty());
    s.f.close().await;
}

#[tokio::test]
async fn a_site_removed_while_its_restarted_server_is_read_or_asked_stays_away() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    let restart = |socket| {
        s.servers.stop(5173);
        s.servers.serve(LOOPBACK, 5173, &s.dir, socket);
    };
    // Restarted, its preview is removed while the new server is asked.
    restart(2);
    s.pages.meanwhile.set(s.removing());
    s.pass().await;
    s.pass().await;
    assert!(s.previews().is_empty());
    // Started again after that, it is found again.
    restart(3);
    s.pass().await;
    assert_eq!(s.previews().len(), 1);
    // Restarted, its preview is removed while the computer is read.
    restart(4);
    s.servers.meanwhile.set(s.removing());
    s.pass().await;
    s.pass().await;
    assert!(s.previews().is_empty());
    restart(5);
    s.pass().await;
    assert_eq!(s.previews().len(), 1);
    s.f.close().await;
}

#[tokio::test]
async fn a_server_working_elsewhere_a_while_is_the_same_server_when_it_returns() {
    let s = Finding::new().await;
    for (port, socket) in [(5173, 1), (8080, 2), (4000, 3)] {
        let title = (port == 5173).then(|| "Shop".to_string());
        s.pages.answer(port, PageAnswer::Page(title));
        s.servers.serve(LOOPBACK, port, &s.dir, socket);
    }
    s.pass().await;
    let preview = |port: u16| {
        let port = format!(":{port}/");
        s.previews().into_iter().find(|p| p.url.ends_with(&port))
    };
    s.remove(preview(8080).unwrap().preview_id).await;
    // Their programs work outside every project for a pass, listening still.
    let elsewhere = Path::new("/elsewhere");
    for port in [5173, 8080, 4000] {
        s.servers.moves(port, elsewhere);
    }
    s.pass().await;
    assert_eq!(preview(5173).unwrap().reachable, Some(false));
    // One removed meanwhile.
    s.remove(preview(4000).unwrap().preview_id).await;
    for port in [5173, 8080, 4000] {
        s.servers.moves(port, &s.dir);
    }
    s.pass().await;
    s.pass().await;
    // Back, each is the server it was: not asked again, and those removed
    // stay away.
    let previews = s.previews();
    assert_eq!(previews.len(), 1);
    assert_eq!(
        (previews[0].label.as_str(), previews[0].reachable),
        ("Shop", Some(true))
    );
    for port in [5173, 8080, 4000] {
        assert_eq!(s.pages.asks(&format!("http://127.0.0.1:{port}/")), 1);
    }
    // Stopped while it works elsewhere, it is found again when it starts.
    s.servers.moves(8080, elsewhere);
    s.pass().await;
    s.servers.stop(8080);
    s.pass().await;
    s.servers.serve(LOOPBACK, 8080, &s.dir, 4);
    s.pass().await;
    assert!(preview(8080).is_some());
    // Removed again and working elsewhere, another server on its port in
    // the project is another, and found.
    s.remove(preview(8080).unwrap().preview_id).await;
    s.servers.moves(8080, elsewhere);
    s.pass().await;
    let other = IpAddr::V6(Ipv6Addr::LOCALHOST);
    s.servers.serve(other, 8080, &s.dir, 5);
    s.pass().await;
    assert_eq!(preview(8080).unwrap().url, "http://[::1]:8080/");
    // That one removed too and stopped, the first stays away when it
    // returns.
    s.remove(preview(8080).unwrap().preview_id).await;
    s.servers.closes(5);
    s.pass().await;
    s.servers.moves(8080, &s.dir);
    s.pass().await;
    s.pass().await;
    assert!(preview(8080).is_none());
    assert_eq!(s.pages.asks("http://127.0.0.1:8080/"), 2);
    s.f.close().await;
}

#[tokio::test]
async fn a_preview_removed_once_ophio_starts_again_keeps_its_server_away_wherever_it_works() {
    let s = Finding::new().await;
    s.pages.answer(8080, PageAnswer::Page(Some("Shop".into())));
    s.servers.serve(LOOPBACK, 8080, &s.dir, 1);
    s.pass().await;
    assert_eq!(s.kept(SERVING), ["first boot"]);
    s.servers.moves(8080, Path::new("/elsewhere"));
    s.pass().await;
    // Started again while its server works elsewhere.
    let s = s.restarted().await;
    s.pass().await;
    s.remove(s.previews()[0].preview_id).await;
    s.servers.moves(8080, &s.dir);
    s.pass().await;
    s.pass().await;
    assert!(s.previews().is_empty());
    assert_eq!(s.pages.asks("http://127.0.0.1:8080/"), 1);
    assert!(s.kept(SERVING).is_empty());
    assert_eq!(s.kept(REMOVALS), ["first boot"]);
    s.f.close().await;
}

#[tokio::test]
async fn a_server_found_on_a_previews_port_that_isnt_a_site_doesnt_take_its_removal() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    // Its server works elsewhere a while, and another on its port in the
    // project isn't a site.
    s.servers.moves(5173, Path::new("/elsewhere"));
    s.pages.answer(5173, PageAnswer::Other);
    s.servers
        .serve(IpAddr::V6(Ipv6Addr::LOCALHOST), 5173, &s.dir, 2);
    s.pass().await;
    let s = s.restarted().await;
    s.pass().await;
    assert_eq!(s.previews()[0].url, "http://127.0.0.1:5173/");
    s.remove(s.previews()[0].preview_id).await;
    s.servers.closes(2);
    s.servers.moves(5173, &s.dir);
    s.pass().await;
    s.pass().await;
    assert!(s.previews().is_empty());
    for url in ["http://127.0.0.1:5173/", "http://[::1]:5173/"] {
        assert_eq!(s.pages.asks(url), 1);
    }
    s.f.close().await;
}

#[tokio::test]
async fn a_preview_another_server_takes_over_is_that_ones_to_remove() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    // Its server works elsewhere, listening still, and another serves the
    // site in its place.
    s.servers.moves(5173, Path::new("/elsewhere"));
    s.servers
        .serve(IpAddr::V6(Ipv6Addr::LOCALHOST), 5173, &s.dir, 2);
    s.pass().await;
    assert_eq!(s.previews()[0].url, "http://[::1]:5173/");
    s.remove(s.previews()[0].preview_id).await;
    s.pass().await;
    assert!(s.previews().is_empty());
    s.f.close().await;
}

#[tokio::test]
async fn a_preview_found_after_one_removed_is_its_own_servers_to_remove() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.pass().await;
    s.remove(s.previews()[0].preview_id).await;
    s.pass().await;
    // The removed one works elsewhere, listening still, and another serves
    // a site on its port.
    s.servers.moves(5173, Path::new("/elsewhere"));
    s.servers
        .serve(IpAddr::V6(Ipv6Addr::LOCALHOST), 5173, &s.dir, 2);
    s.pass().await;
    assert_eq!(s.previews()[0].url, "http://[::1]:5173/");
    s.remove(s.previews()[0].preview_id).await;
    s.pass().await;
    assert!(s.previews().is_empty());
    s.f.close().await;
}

#[tokio::test]
async fn what_is_kept_of_servers_that_run_on_has_a_limit() {
    let s = Finding::new().await;
    // As many servers as answers are kept, each working elsewhere once asked.
    let ports = 10_000..10_000 + ANSWERS_KEPT as u16;
    for port in ports.clone() {
        s.pages.answer(port, PageAnswer::Other);
        s.servers.serve(LOOPBACK, port, &s.dir, u64::from(port));
    }
    for _ in 0..ANSWERS_KEPT.div_ceil(ASKED_AT_ONCE) {
        s.pass().await;
    }
    let elsewhere = Path::new("/elsewhere");
    for port in ports.clone() {
        assert_eq!(s.pages.asks(&format!("http://127.0.0.1:{port}/")), 1);
        s.servers.moves(port, elsewhere);
    }
    // Others aren't asked while they run, so none is asked again.
    let title = "Long".repeat(NAME_BYTES / 4) + "!";
    s.pages.answer(3000, PageAnswer::Page(Some(title)));
    s.pages.answer(3001, PageAnswer::Page(None));
    s.servers.serve(LOOPBACK, 3000, &s.dir, 1);
    s.servers.serve(LOOPBACK, 3001, &s.dir, 2);
    s.pass().await;
    s.pass().await;
    let asks = || [3000, 3001].map(|port| s.pages.asks(&format!("http://127.0.0.1:{port}/")));
    assert_eq!(asks(), [0, 0]);
    assert_eq!(s.kept(ANSWERS).len(), ANSWERS_KEPT);
    // One stopped, one is asked in its place. A name longer than any is
    // none.
    s.servers.closes(10_000);
    s.pass().await;
    assert_eq!(asks(), [1, 0]);
    assert_eq!(s.previews()[0].label, "127.0.0.1:3000");
    for port in ports.skip(1) {
        s.servers.moves(port, &s.dir);
    }
    s.pass().await;
    assert_eq!(s.kept(ANSWERS).len(), ANSWERS_KEPT);
    assert_eq!(s.pages.asks("http://127.0.0.1:10001/"), 1);
    assert_eq!(asks(), [1, 0]);
    s.f.close().await;
}

#[tokio::test]
async fn no_more_answers_are_read_back_than_are_kept() {
    let s = Finding::new().await;
    s.pages.answer(9229, PageAnswer::Other);
    s.servers.serve(LOOPBACK, 9229, &s.dir, 1);
    s.pass().await;
    // More kept than there may be, by an Ophio before, of servers running
    // still.
    let kept: Vec<(String, serde_json::Value)> = s.f.hub.read(|r| r.entries(ANSWERS)).unwrap();
    let more = 2..=ANSWERS_KEPT as u64 + 1;
    s.f.hub
        .write(|tx| {
            for socket in more.clone() {
                tx.put(ANSWERS, socket.to_string(), &kept[0].1)?;
            }
            Ok(())
        })
        .unwrap();
    for socket in more {
        s.servers
            .serve(LOOPBACK, 9229, Path::new("/elsewhere"), socket);
    }
    let s = s.restarted().await;
    s.pass().await;
    assert_eq!(s.kept(ANSWERS).len(), ANSWERS_KEPT);
    s.f.close().await;
}

#[tokio::test]
async fn what_servers_answered_and_were_removed_outlasts_the_companion_in_its_boot() {
    let s = Finding::new().await;
    s.pages.answer(5173, PageAnswer::Page(Some("Shop".into())));
    s.pages.answer(8080, PageAnswer::Page(None));
    s.pages.answer(9229, PageAnswer::Other);
    s.servers.serve(LOOPBACK, 5173, &s.dir, 1);
    s.servers.serve(LOOPBACK, 8080, &s.dir, 2);
    s.servers.serve(LOOPBACK, 9229, &s.dir, 3);
    s.pass().await;
    let removed = s.previews()[1].clone();
    assert!(removed.url.ends_with(":8080/"));
    s.remove(removed.preview_id).await;
    let asks = |s: &Finding| {
        [5173, 8080, 9229].map(|port| s.pages.asks(&format!("http://127.0.0.1:{port}/")))
    };
    let labels = |s: &Finding| {
        (s.previews().into_iter())
            .map(|p| p.label)
            .collect::<Vec<_>>()
    };

    // Started again in the same boot, with the servers running on.
    let s = s.restarted().await;
    s.pass().await;
    assert_eq!(labels(&s), ["Shop"]);
    assert_eq!(asks(&s), [1, 1, 1]);
    // A preview removed before the first pass stays away too.
    let s = s.restarted().await;
    s.remove(s.previews()[0].preview_id).await;
    s.pass().await;
    s.pass().await;
    assert!(s.previews().is_empty());
    assert_eq!(asks(&s), [1, 1, 1]);
    assert_eq!(s.kept(ANSWERS).len(), 3);
    assert_eq!(s.kept(REMOVALS).len(), 2);

    // In another boot the sockets' numbers are others': each is asked again.
    *s.servers.boot.lock().unwrap() = Some("second boot".into());
    let s = s.restarted().await;
    s.pass().await;
    assert_eq!(labels(&s), ["Shop", "127.0.0.1:8080"]);
    assert_eq!(asks(&s), [2, 2, 2]);
    assert_eq!(s.kept(ANSWERS), ["second boot"; 3]);
    assert!(s.kept(REMOVALS).is_empty());
    // A stopped server's is let go.
    s.servers.stop(9229);
    s.pass().await;
    assert_eq!(s.kept(ANSWERS).len(), 2);
    // Where the boot isn't known, nothing is kept.
    *s.servers.boot.lock().unwrap() = None;
    let s = s.restarted().await;
    s.pass().await;
    assert_eq!(asks(&s), [3, 3, 2]);
    assert!(s.kept(ANSWERS).is_empty());
    s.f.close().await;
}

#[tokio::test]
async fn a_server_that_never_answers_is_asked_so_many_times_however_often_ophio_starts() {
    let mut s = Finding::new().await;
    s.servers.serve(LOOPBACK, 3000, &s.dir, 1);
    for _ in 0..ASKS + 2 {
        s.pass().await;
        s = s.restarted().await;
    }
    s.pass().await;
    assert_eq!(s.pages.asks("http://127.0.0.1:3000/"), usize::from(ASKS));
    assert!(s.previews().is_empty());
    s.f.close().await;
}
