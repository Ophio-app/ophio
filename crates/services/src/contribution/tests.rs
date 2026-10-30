// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn rejects_private_addresses_and_ambiguous_paths() {
    for ip in [
        "0.1.2.3",
        "10.0.0.1",
        "127.0.0.1",
        "100.64.0.1",
        "169.254.169.254",
        "172.31.1.1",
        "192.168.1.1",
        "192.0.0.1",
        "192.0.2.1",
        "192.88.99.1",
        "192.31.196.1",
        "192.52.193.1",
        "192.175.48.1",
        "198.19.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "255.255.255.255",
        "::1",
        "::ffff:8.8.8.8",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
        "2002::1",
        "3fff::1",
        "2620:4f:8000::1",
    ] {
        assert!(!network::public(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
        assert!(network::public(ip.parse().unwrap()));
    }
    for path in [
        "",
        "/absolute",
        "../outside",
        "a/../b",
        "a//b",
        ".git/config",
        ".GIT/config",
        ".gitmodules",
        "a\\b",
        "C:drive",
        "a.",
        "a ",
        "a/CON.txt",
        "a/COM1",
        "git~1/config",
        "é.txt",
    ] {
        assert!(validate::path(path).is_err(), "{path}");
    }
    for url in [
        "file:///repo",
        "ssh://example.com/repo",
        "https://u:p@example.com/repo",
        "https://127.0.0.1/repo",
        "https://example.com:443/repo",
        "https://example.com/a/../b",
        "https://example.com/a%2fb",
        "https://example.com/repo?token=x",
        "https://example.com/repo#x",
        "https://private.local/repo",
    ] {
        assert!(validate::repository(url).is_err(), "{url}");
    }
    assert!(validate::repository("https://github.com/fixture/source").is_ok());
}

#[derive(Debug)]
struct FixturePack {
    bytes: Vec<u8>,
    commit: String,
    policies: Vec<validate::Policy>,
}
fn object(kind: u8, bytes: Vec<u8>) -> (String, u8, Vec<u8>) {
    let head = format!("{} {}\0", pack::kind(kind).unwrap(), bytes.len());
    (
        pack::hex(&pack::sha1(&[head.as_bytes(), &bytes], &|| Ok(())).unwrap()),
        kind,
        bytes,
    )
}
fn pack_bytes(objects: &[(String, u8, Vec<u8>)]) -> Vec<u8> {
    use std::io::Write;
    let mut pack = b"PACK\0\0\0\x02".to_vec();
    pack.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    for (_, kind, bytes) in objects {
        let mut n = bytes.len();
        let mut first = (*kind << 4) | (n as u8 & 15);
        n >>= 4;
        if n != 0 {
            first |= 128;
        }
        pack.push(first);
        while n != 0 {
            let mut b = n as u8 & 127;
            n >>= 7;
            if n != 0 {
                b |= 128;
            }
            pack.push(b);
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(bytes).unwrap();
        pack.extend(z.finish().unwrap());
    }
    pack.extend_from_slice(&pack::sha1(&[&pack], &|| Ok(())).unwrap());
    pack
}
fn fixture(extra: &[(&str, &str, &[u8])]) -> FixturePack {
    let definitions = [
        (
            "AGENTS.md",
            "instructions",
            b"Owned fixture instructions.\n".as_slice(),
        ),
        (
            "CONTRIBUTING.md",
            "contribution",
            b"Owned fixture contribution rules.\n".as_slice(),
        ),
        (
            "POLICY.md",
            "model-policy",
            b"Owned fixture model policy.\n".as_slice(),
        ),
    ];
    let mut objects = Vec::new();
    let mut entries = Vec::new();
    let mut policies = Vec::new();
    for (name, role, bytes) in definitions {
        let blob = object(3, bytes.to_vec());
        entries.push((name, "100644", blob.0.clone()));
        policies.push(validate::Policy {
            path: name.into(),
            sha256: validate::digest(bytes),
            role: role.into(),
        });
        objects.push(blob);
    }
    for (name, mode, bytes) in extra {
        let blob = object(3, bytes.to_vec());
        entries.push((name, mode, blob.0.clone()));
        objects.push(blob);
    }
    entries.sort();
    let mut tree = Vec::new();
    for (name, mode, hash) in entries {
        tree.extend_from_slice(format!("{mode} {name}\0").as_bytes());
        for i in (0..40).step_by(2) {
            tree.push(u8::from_str_radix(&hash[i..i + 2], 16).unwrap());
        }
    }
    let tree = object(2, tree);
    let commit = object(1,format!("tree {}\nauthor Fixture <fixture@example.invalid> 1700000000 +0000\ncommitter Fixture <fixture@example.invalid> 1700000000 +0000\n\nOwned fixture\n",tree.0).into_bytes());
    let sha = commit.0.clone();
    objects.push(tree);
    objects.push(commit);
    FixturePack {
        bytes: pack_bytes(&objects),
        commit: sha,
        policies,
    }
}
fn input(
    pack: &FixturePack,
    host_id: HostId,
    root_id: LibraryRootId,
    now: u64,
    packet_id: &str,
) -> ContributionInput {
    let h = serde_json::json!({"schemaVersion":1,"gameId":"fixture","releaseId":"one","source":{"repositoryUrl":"https://source.example/owned/fixture","commit":pack.commit},"policies":pack.policies,
        "compatibility":{"receiver":"ophio","protocolVersion":CONTRACT,"requiredCapabilities":["pinned-import"]},"trust":{"reviewRequired":true,"isolatedWorkspaceRequired":true,"execution":"explicit-approval"}}).to_string();
    let p = serde_json::json!({"schemaVersion":1,"kind":"contribution-import","packetId":packet_id,"issuedAt":now,"expiresAt":now+3600,"handoffSha256":validate::digest(&h)}).to_string();
    ContributionInput {
        packet_json: p,
        handoff_json: h,
        host_id,
        root_id,
    }
}
fn approval(i: &ContributionInput, now: u64) -> ContributionApproval {
    let (_, h) = validate::records(&i.packet_json, &i.handoff_json).unwrap();
    ContributionApproval {
        display_name: None,
        publisher_id: "owned-reviewer".into(),
        packet_sha256: validate::digest(&i.packet_json),
        handoff_sha256: validate::digest(&i.handoff_json),
        game_id: h.game_id,
        release_id: h.release_id,
        repository_url: h.source.repository_url,
        commit: h.source.commit,
        checked_at: now,
        valid_until: now + 300,
    }
}
#[test]
fn exact_bytes_closed_records_expiry_and_scoped_approval() {
    let p = fixture(&[]);
    let i = input(&p, HostId::new(), LibraryRootId::new(), 1000, "packet");
    let (packet, h) = validate::records(&i.packet_json, &i.handoff_json).unwrap();
    let a = approval(&i, 1000);
    assert!(validate::approval(&packet, &h, &validate::digest(&i.packet_json), &a, 1000).is_ok());
    assert_eq!(validate::lifetime(&packet, 4600), Err(Code::Expired));
    assert_eq!(validate::lifetime(&packet, 999), Err(Code::NotYetValid));
    assert_eq!(
        validate::approval(&packet, &h, &validate::digest(&i.packet_json), &a, 1300),
        Err(Code::FreshnessUnknown)
    );
    assert_eq!(
        validate::records(&i.packet_json, &format!("{}\n", i.handoff_json)).unwrap_err(),
        Code::HandoffMismatch
    );
    for field in ["token", "workspace", "command", "startApproved"] {
        let mut p: serde_json::Value = serde_json::from_str(&i.packet_json).unwrap();
        p[field] = serde_json::json!(true);
        assert_eq!(
            validate::records(&p.to_string(), &i.handoff_json).unwrap_err(),
            Code::Malformed
        );
    }
    let mut a = a;
    a.commit = "a".repeat(40);
    assert_eq!(
        validate::approval(&packet, &h, &validate::digest(&i.packet_json), &a, 1000),
        Err(Code::IdentityMismatch)
    );
    let huge = " ".repeat(RECORD_BYTES + 1);
    assert_eq!(
        validate::records(&huge, "{}").unwrap_err(),
        Code::LimitExceeded
    );
    let duplicate = i.packet_json.replacen('{', "{\"schemaVersion\":0,", 1);
    assert_eq!(
        validate::records(&duplicate, &i.handoff_json)
            .unwrap()
            .0
            .schema_version,
        1
    );
}
#[test]
fn catalog_display_name_requires_bounded_independent_approval() {
    let p = fixture(&[]);
    let i = input(&p, HostId::new(), LibraryRootId::new(), 1000, "packet");
    let (packet, handoff) = validate::records(&i.packet_json, &i.handoff_json).unwrap();
    let mut a = approval(&i, 1000);
    for name in [
        "".into(),
        " ".into(),
        "x".repeat(121),
        "/private/name".into(),
        "game\nname".into(),
        "game\u{202e}name".into(),
    ] {
        a.display_name = Some(name);
        assert_eq!(
            validate::approval(&packet, &handoff, &a.packet_sha256, &a, 1000),
            Err(Code::Malformed)
        );
    }
    a.display_name = Some("A lovely game".into());
    assert!(validate::approval(&packet, &handoff, &a.packet_sha256, &a, 1000).is_ok());
    let mut h: serde_json::Value = serde_json::from_str(&i.handoff_json).unwrap();
    h["displayName"] = serde_json::json!("Packet-supplied name");
    assert_eq!(
        validate::records(&i.packet_json, &h.to_string()).unwrap_err(),
        Code::Malformed
    );
}

#[test]
fn hostile_git_objects_never_become_files() {
    for (name, mode, bytes) in [
        (".git", "100644", b"malicious config".as_slice()),
        (".GIT", "100644", b"config"),
        (".gitmodules", "100644", b"[submodule]"),
        ("../escape", "100644", b"escape"),
        ("symlink", "120000", b"../outside"),
        ("submodule", "160000", b"x"),
        ("agents.md", "100644", b"case collision"),
        (
            "asset",
            "100644",
            b"version https://git-lfs.github.com/spec/v1\n",
        ),
    ] {
        let f = fixture(&[(name, mode, bytes)]);
        assert!(
            pack::decode(&f.bytes, &f.commit, &|| Ok(())).is_err(),
            "{name}"
        );
    }
    let f = fixture(&[]);
    assert_eq!(
        pack::decode(&f.bytes, &"a".repeat(40), &|| Ok(())).unwrap_err(),
        Code::IdentityMismatch
    );
    assert_eq!(
        pack::decode(&f.bytes, &f.commit, &|| Err(Code::Canceled)).unwrap_err(),
        Code::Canceled
    );
    for n in 0..f.bytes.len() {
        assert!(pack::decode(&f.bytes[..n], &f.commit, &|| Ok(())).is_err());
    }
    let mut corrupt = f.bytes.clone();
    corrupt[20] ^= 1;
    assert_eq!(
        pack::decode(&corrupt, &f.commit, &|| Ok(())).unwrap_err(),
        Code::UnsafeObject
    );
}

#[cfg(target_os = "linux")]
mod engine {
    use super::*;
    use crate::{Hub, testing::FakeClock};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Semaphore;
    #[derive(Debug)]
    struct DeliverySource {
        bytes: Vec<u8>,
        entered: Semaphore,
        proceed: Semaphore,
    }
    #[async_trait::async_trait]
    impl network::Fetch for DeliverySource {
        async fn handoff(&self, _: &str) -> Result<Vec<u8>, Code> {
            self.entered.add_permits(1);
            self.proceed.acquire().await.unwrap().forget();
            Ok(self.bytes.clone())
        }
        async fn pack(&self, _: &str, _: &str, _: &AtomicU64) -> Result<Vec<u8>, Code> {
            panic!("delivery must never acquire source");
        }
    }
    fn delivery(f: &Fixture, raw: &str) -> Query {
        let packet = serde_json::json!({"schemaVersion":1,"kind":"contribution-import","packetId":"delivery",
            "issuedAt":seconds(&f.hub),"expiresAt":seconds(&f.hub)+600,"handoffSha256":validate::digest(raw)});
        Query::ContributionDelivery {
            packet_json: format!(" {packet}\n"),
            handoff_url: "https://catalog.example.com/handoff.json".into(),
        }
    }
    fn delivery_source(f: &Fixture, bytes: Vec<u8>) -> Arc<DeliverySource> {
        let source = Arc::new(DeliverySource {
            bytes,
            entered: Semaphore::new(0),
            proceed: Semaphore::new(1),
        });
        *f.hub.inner.contributions.source.lock().unwrap() = source.clone();
        source
    }
    #[tokio::test]
    async fn delivery_preserves_full_record_bytes_without_authority_or_writes() {
        let f = Fixture::new().await;
        let raw = format!(" {}\n", f.input.handoff_json);
        let raw = format!("{raw}{}", " ".repeat(RECORD_BYTES - raw.len()));
        let source = delivery_source(&f, raw.as_bytes().to_vec());
        for _ in 0..2 {
            let response = f
                .hub
                .query(&Caller::local_owner(), delivery(&f, &raw))
                .await
                .unwrap();
            assert_eq!(
                response,
                QueryData::ContributionDelivery {
                    handoff_json: raw.clone()
                }
            );
            source.proceed.add_permits(1);
        }
        f.hub
            .read(|r| {
                assert!(r.entries::<Record>("contribution")?.is_empty());
                assert!(r.entries::<Project>("project")?.is_empty());
                assert!(r.entries::<QueueView>("queue")?.is_empty());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            std::fs::read_dir(f.temp.path().join("projects"))
                .unwrap()
                .count(),
            0
        );
    }
    #[tokio::test]
    async fn delivery_rejects_denied_unsafe_changed_and_oversized_records() {
        let f = Fixture::new().await;
        let mut caller = Caller::local_owner();
        caller.grants.admin = false;
        assert_eq!(
            f.hub
                .query(&caller, delivery(&f, &f.input.handoff_json))
                .await
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        for url in [
            "http://example.com/h.json",
            "https://127.0.0.1/h.json",
            "https://user@example.com/h.json",
            "https://example.com/a/../h.json",
        ] {
            let Query::ContributionDelivery { packet_json, .. } =
                delivery(&f, &f.input.handoff_json)
            else {
                unreachable!()
            };
            assert!(
                f.hub
                    .query(
                        &Caller::local_owner(),
                        Query::ContributionDelivery {
                            packet_json,
                            handoff_url: url.into()
                        }
                    )
                    .await
                    .is_err()
            );
        }
        for (bytes, expected) in [
            (
                format!("{} ", f.input.handoff_json).into_bytes(),
                "handoff_digest_mismatch",
            ),
            (vec![b' '; RECORD_BYTES + 1], "limit_exceeded"),
            (vec![0xff], "invalid_json"),
        ] {
            delivery_source(&f, bytes);
            assert_eq!(
                f.hub
                    .query(&Caller::local_owner(), delivery(&f, &f.input.handoff_json))
                    .await
                    .unwrap_err()
                    .message,
                expected
            );
        }
    }
    #[tokio::test]
    async fn delivery_rechecks_expiry_after_fetch() {
        let f = Fixture::new().await;
        let source = delivery_source(&f, f.input.handoff_json.as_bytes().to_vec());
        source.proceed.acquire().await.unwrap().forget();
        let query = delivery(&f, &f.input.handoff_json);
        let caller = Caller::local_owner();
        let fetch = f.hub.query(&caller, query);
        let expire = async {
            source.entered.acquire().await.unwrap().forget();
            f.clock.advance(Duration::from_secs(600));
            source.proceed.add_permits(1);
        };
        let (result, ()) = tokio::join!(fetch, expire);
        assert_eq!(result.unwrap_err().message, "expired");
    }
    #[derive(Debug)]
    struct Source {
        bytes: Vec<u8>,
        calls: AtomicUsize,
        entered: Semaphore,
        proceed: Semaphore,
    }
    #[async_trait::async_trait]
    impl network::Fetch for Source {
        async fn pack(
            &self,
            _: &str,
            _: &str,
            received: &std::sync::atomic::AtomicU64,
        ) -> Result<Vec<u8>, Code> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            received.store(self.bytes.len() as u64, Ordering::Relaxed);
            self.entered.add_permits(1);
            self.proceed.acquire().await.unwrap().forget();
            Ok(self.bytes.clone())
        }
    }
    struct Fixture {
        temp: tempfile::TempDir,
        hub: CompanionHub,
        clock: Arc<FakeClock>,
        source: Arc<Source>,
        input: ContributionInput,
    }
    impl Fixture {
        async fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("projects");
            std::fs::create_dir(&root).unwrap();
            let clock = Arc::new(FakeClock::default());
            let hub = CompanionHub::builder()
                .data_dir(temp.path().join("data"))
                .clock(clock.clone())
                .build()
                .await
                .unwrap();
            let root_id = LibraryRootId::new();
            hub.write(|tx| {
                tx.put(
                    "meta",
                    "library_roots",
                    &vec![LibraryRoot {
                        root_id,
                        label: "fixture".into(),
                        host_path: root.to_str().unwrap().into(),
                        kind: protocol::library::RootKind::Project,
                        writable: true,
                        project_id: None,
                        harness: None,
                        only: None,
                    }],
                )
            })
            .unwrap();
            let pack = fixture(&[
                ("work.txt", "100644", b"original\n"),
                ("setup.sh", "100755", b"#!/bin/sh\nexit 99\n"),
                (".gitattributes", "100644", b"* filter=fixture\n"),
            ]);
            let input = input(&pack, hub.inner.host_id, root_id, seconds(&hub), "packet");
            let source = Arc::new(Source {
                bytes: pack.bytes,
                calls: AtomicUsize::new(0),
                entered: Semaphore::new(0),
                proceed: Semaphore::new(0),
            });
            *hub.inner.contributions.source.lock().unwrap() = source.clone();
            Self {
                temp,
                hub,
                clock,
                source,
                input,
            }
        }
        async fn submit(&self, cmd: Command) -> ContributionView {
            let r = self
                .hub
                .submit(&Caller::local_owner(), CommandEnvelope::new(cmd))
                .await;
            assert!(r.error.is_none(), "{r:?}");
            let Some(CommandResult::Contribution { operation }) = r.result else {
                panic!("{r:?}");
            };
            operation
        }
        async fn review(&self) -> ContributionView {
            self.submit(Command::ReviewContribution {
                input: Box::new(self.input.clone()),
            })
            .await
        }
        async fn prepare(&self, id: &str) -> ContributionView {
            self.submit(Command::PrepareContribution {
                workspace_choice: None,
                operation_id: id.into(),
                approval: Box::new(approval(&self.input, seconds(&self.hub))),
            })
            .await
        }
        async fn complete(&self, id: &str) -> ContributionView {
            self.complete_choice(id, None).await
        }
        async fn complete_choice(
            &self,
            id: &str,
            choice: Option<ContributionWorkspaceChoice>,
        ) -> ContributionView {
            let mut sub = self
                .hub
                .subscribe(&Caller::local_owner(), None)
                .await
                .unwrap();
            let admitted = self
                .submit(Command::PrepareContribution {
                    operation_id: id.into(),
                    approval: Box::new(approval(&self.input, seconds(&self.hub))),
                    workspace_choice: choice,
                })
                .await;
            if !matches!(admitted.code, Code::Acquiring | Code::Verifying) {
                return admitted;
            }
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let crate::api::Outbound::Event(e) = sub.live.recv().await.unwrap()
                        && let Event::CommandProgress {
                            result: Some(CommandResult::Contribution { operation }),
                            ..
                        } = &e.event
                        && operation.operation_id == admitted.operation_id
                        && !matches!(operation.code, Code::Acquiring | Code::Verifying)
                    {
                        return operation.clone();
                    }
                }
            })
            .await
            .unwrap()
        }
        fn record(&self, id: &str) -> Record {
            self.hub.read(|r| r.require("contribution", id)).unwrap()
        }
        fn git(&self, path: &std::path::Path, args: &[&str]) -> std::process::Output {
            let out = std::process::Command::new("git")
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", self.temp.path())
                .arg("-C")
                .arg(path)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            out
        }
        async fn restart_before_registration(self, done: &ContributionView) -> Self {
            let roots: Vec<LibraryRoot> = self
                .hub
                .read(|r| r.require("meta", "library_roots"))
                .unwrap();
            self.hub
                .write(|tx| {
                    let mut rec: Record = tx.read().require("contribution", &done.operation_id)?;
                    tx.delete("project", done.project_id.unwrap())?;
                    tx.delete("queue", done.project_id.unwrap())?;
                    rec.view.project_id = None;
                    rec.view.code = Code::Verifying;
                    rec.active = true;
                    save(tx, &rec)
                })
                .unwrap();
            let Self {
                temp,
                hub,
                clock,
                source,
                input,
            } = self;
            hub.shutdown().await.unwrap();
            drop(hub);
            let hub = CompanionHub::builder()
                .data_dir(temp.path().join("data"))
                .clock(clock.clone())
                .build()
                .await
                .unwrap();
            *hub.inner.contributions.source.lock().unwrap() = source.clone();
            hub.write(|tx| tx.put("meta", "library_roots", &roots))
                .unwrap();
            Self {
                temp,
                hub,
                clock,
                source,
                input,
            }
        }
        async fn close(self) {
            self.hub.shutdown().await.unwrap();
        }
    }
    fn rewrite_handoff(input: &mut ContributionInput, mutate: impl FnOnce(&mut serde_json::Value)) {
        let mut handoff: serde_json::Value = serde_json::from_str(&input.handoff_json).unwrap();
        mutate(&mut handoff);
        input.handoff_json = handoff.to_string();
        let mut packet: serde_json::Value = serde_json::from_str(&input.packet_json).unwrap();
        packet["packetId"] = serde_json::json!("renewed-packet");
        packet["handoffSha256"] = serde_json::json!(validate::digest(&input.handoff_json));
        input.packet_json = packet.to_string();
    }
    #[tokio::test]
    async fn policy_preview_reads_pinned_text_without_approval_or_workspace() {
        let f = Fixture::new().await;
        let reviewed = f.review().await;
        assert!(f.record(&reviewed.operation_id).approval.is_none());
        f.source.proceed.add_permits(1);
        let response = f
            .hub
            .query(
                &Caller::local_owner(),
                Query::ContributionPolicy {
                    operation_id: reviewed.operation_id.clone(),
                    path: "AGENTS.md".into(),
                },
            )
            .await
            .unwrap();
        let QueryData::ContributionPolicy { document } = response else {
            panic!("{response:?}");
        };
        assert_eq!(document.text, "Owned fixture instructions.\n");
        assert_eq!(document.size_bytes as usize, document.text.len());
        assert_eq!(document.policy.sha256, validate::digest(&document.text));
        let rec = f.record(&reviewed.operation_id);
        assert!(rec.approval.is_none() && rec.workspace.is_none() && rec.manifest.is_none());
        assert_eq!(rec.view, reviewed);
        assert_eq!(
            std::fs::read_dir(f.temp.path().join("projects"))
                .unwrap()
                .count(),
            0
        );
        assert!(
            f.hub
                .read(|r| r.list::<Project>("project"))
                .unwrap()
                .is_empty()
        );
        assert!(
            f.hub
                .read(|r| r.list::<QueueView>("queue"))
                .unwrap()
                .is_empty()
        );
        assert!(
            f.hub
                .query(
                    &Caller::local_owner(),
                    Query::ContributionPolicy {
                        operation_id: reviewed.operation_id.clone(),
                        path: "work.txt".into(),
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        let denied = Caller {
            grants: auth::Grants {
                admin: false,
                ..auth::Grants::owner()
            },
            ..Caller::local_owner()
        };
        assert!(
            f.hub
                .query(
                    &denied,
                    Query::ContributionPolicy {
                        operation_id: reviewed.operation_id.clone(),
                        path: "AGENTS.md".into(),
                    }
                )
                .await
                .is_err()
        );
        f.hub
            .write(|tx| {
                let mut rec: Record = tx.read().require("contribution", &reviewed.operation_id)?;
                rec.owner = Principal::Device {
                    device_id: DeviceId::new(),
                };
                save(tx, &rec)
            })
            .unwrap();
        assert!(
            f.hub
                .query(
                    &Caller::local_owner(),
                    Query::ContributionPolicy {
                        operation_id: reviewed.operation_id,
                        path: "AGENTS.md".into(),
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        f.close().await;
    }

    #[tokio::test]
    async fn policy_preview_checks_all_pins_and_refuses_canceled_operations() {
        let mut f = Fixture::new().await;
        rewrite_handoff(&mut f.input, |h| {
            h["policies"][1]["sha256"] = serde_json::json!("0".repeat(64))
        });
        let reviewed = f.review().await;
        f.source.proceed.add_permits(1);
        let q = Query::ContributionPolicy {
            operation_id: reviewed.operation_id.clone(),
            path: "AGENTS.md".into(),
        };
        let e = f
            .hub
            .query(&Caller::local_owner(), q.clone())
            .await
            .unwrap_err();
        assert_eq!(e.message, "stale_policy");
        f.submit(Command::CancelContribution {
            operation_id: reviewed.operation_id.clone(),
        })
        .await;
        assert!(f.hub.query(&Caller::local_owner(), q).await.is_err());
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        f.close().await;
    }

    #[tokio::test]
    async fn canceled_handoff_can_be_reviewed_and_read_again_before_trust() {
        let f = Fixture::new().await;
        let original = f.review().await;
        f.submit(Command::CancelContribution {
            operation_id: original.operation_id.clone(),
        })
        .await;
        let reviewed = f.review().await;
        assert_eq!(reviewed.code, Code::AwaitingLocalApproval);
        f.source.proceed.add_permits(1);
        let response = f
            .hub
            .query(
                &Caller::local_owner(),
                Query::ContributionPolicy {
                    operation_id: reviewed.operation_id.clone(),
                    path: "AGENTS.md".into(),
                },
            )
            .await
            .unwrap();
        let QueryData::ContributionPolicy { document } = response else {
            panic!()
        };
        assert_eq!(document.text, "Owned fixture instructions.\n");
        assert!(f.record(&reviewed.operation_id).approval.is_none());
        assert_ne!(reviewed.operation_id, original.operation_id);
        assert_eq!(f.record(&original.operation_id).view.code, Code::Canceled);
        assert_eq!(
            std::fs::read_dir(f.temp.path().join("projects"))
                .unwrap()
                .count(),
            0
        );
        f.close().await;
    }

    #[tokio::test]
    async fn fresh_review_keeps_an_earlier_policy_preview_canceled() {
        let f = Fixture::new().await;
        let original = f.review().await;
        let caller = Caller::local_owner();
        let pending = f.hub.query(
            &caller,
            Query::ContributionPolicy {
                operation_id: original.operation_id.clone(),
                path: "AGENTS.md".into(),
            },
        );
        let review_again = async {
            f.source.entered.acquire().await.unwrap().forget();
            f.submit(Command::CancelContribution {
                operation_id: original.operation_id.clone(),
            })
            .await;
            let reviewed = f.review().await;
            f.source.proceed.add_permits(1);
            reviewed
        };
        let (old_result, reviewed) = tokio::join!(pending, review_again);
        assert!(old_result.is_err());
        assert_ne!(reviewed.operation_id, original.operation_id);
        assert_eq!(reviewed.code, Code::AwaitingLocalApproval);
        assert!(f.record(&reviewed.operation_id).approval.is_none());
        f.close().await;
    }

    #[tokio::test]
    async fn preparing_again_does_not_revive_a_canceled_policy_preview() {
        let f = Fixture::new().await;
        let original = f.review().await;
        f.source.proceed.add_permits(1);
        let prepared = f.complete(&original.operation_id).await;
        assert_eq!(prepared.code, Code::Prepared);
        f.source.entered.acquire().await.unwrap().forget();
        let caller = Caller::local_owner();
        let preview = f.hub.query(
            &caller,
            Query::ContributionPolicy {
                operation_id: prepared.operation_id.clone(),
                path: "AGENTS.md".into(),
            },
        );
        let prepare_again = async {
            f.source.entered.acquire().await.unwrap().forget();
            f.submit(Command::CancelContribution {
                operation_id: prepared.operation_id.clone(),
            })
            .await;
            let reviewed = f.review().await;
            let reopened = f.complete(&reviewed.operation_id).await;
            assert_eq!(reopened.code, Code::Reopened);
            assert_eq!(reopened.operation_id, prepared.operation_id);
            f.source.proceed.add_permits(1);
        };
        let (result, ()) = tokio::join!(preview, prepare_again);
        assert!(result.is_err());
        f.close().await;
    }

    #[tokio::test]
    async fn fresh_review_preserves_acquisition_cancellation_until_acknowledged() {
        let f = Fixture::new().await;
        let original = f.review().await;
        f.hub
            .write(|tx| {
                let mut rec: Record = tx.read().require("contribution", &original.operation_id)?;
                rec.active = true;
                rec.command = Some(CommandId::new());
                rec.view.code = Code::Acquiring;
                save(tx, &rec)
            })
            .unwrap();
        let canceled = f
            .submit(Command::CancelContribution {
                operation_id: original.operation_id.clone(),
            })
            .await;
        assert!(canceled.interruption_pending);
        assert_eq!(f.review().await, canceled);
        assert_eq!(f.record(&original.operation_id).view, canceled);
        f.hub
            .write(|tx| {
                let mut rec: Record = tx.read().require("contribution", &original.operation_id)?;
                rec.active = false;
                rec.view.interruption_pending = false;
                save(tx, &rec)
            })
            .unwrap();
        let reviewed = f.review().await;
        assert_ne!(reviewed.operation_id, original.operation_id);
        assert_eq!(reviewed.code, Code::AwaitingLocalApproval);
        f.close().await;
    }

    #[tokio::test]
    async fn policy_preview_bounds_readable_text_and_refuses_expired_packets() {
        for (bytes, expected) in [
            (vec![0xff], Some("unsafe_object")),
            (b"binary\0rule".to_vec(), Some("unsafe_object")),
            (vec![b'a'; 256 * 1024 + 1], Some("stale_policy")),
            (vec![b'\t'; 256 * 1024], None),
        ] {
            let mut f = Fixture::new().await;
            let pack = fixture(&[("RULE.md", "100644", &bytes)]);
            f.input = input(
                &pack,
                f.hub.inner.host_id,
                f.input.root_id,
                seconds(&f.hub),
                "packet",
            );
            rewrite_handoff(&mut f.input, |h| {
                h["policies"][0]["path"] = serde_json::json!("RULE.md");
                h["policies"][0]["sha256"] = serde_json::json!(validate::digest(&bytes));
            });
            f.source = Arc::new(Source {
                bytes: pack.bytes,
                calls: AtomicUsize::new(0),
                entered: Semaphore::new(0),
                proceed: Semaphore::new(1),
            });
            *f.hub.inner.contributions.source.lock().unwrap() = f.source.clone();
            let reviewed = f.review().await;
            let q = Query::ContributionPolicy {
                operation_id: reviewed.operation_id,
                path: "RULE.md".into(),
            };
            let result = f.hub.query(&Caller::local_owner(), q.clone()).await;
            if let Some(expected) = expected {
                assert_eq!(result.unwrap_err().message, expected);
            } else {
                let data = result.unwrap();
                assert!(serde_json::to_vec(&data).unwrap().len() < 1024 * 1024);
                let QueryData::ContributionPolicy { document } = data else {
                    panic!()
                };
                assert_eq!(document.text.as_bytes(), bytes);
            }
            f.clock.advance(Duration::from_secs(86_400));
            assert_eq!(
                f.hub
                    .query(&Caller::local_owner(), q)
                    .await
                    .unwrap_err()
                    .message,
                "expired"
            );
            assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
            f.close().await;
        }
    }

    #[tokio::test]
    async fn policy_preview_stops_when_the_root_is_revoked_during_fetch() {
        let f = Fixture::new().await;
        let reviewed = f.review().await;
        let caller = Caller::local_owner();
        let query = f.hub.query(
            &caller,
            Query::ContributionPolicy {
                operation_id: reviewed.operation_id,
                path: "AGENTS.md".into(),
            },
        );
        let revoke = async {
            f.source.entered.acquire().await.unwrap().forget();
            f.hub
                .write(|tx| tx.put("meta", "library_roots", &Vec::<LibraryRoot>::new()))
                .unwrap();
            f.source.proceed.add_permits(1);
        };
        let (result, ()) = tokio::join!(query, revoke);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_dir(f.temp.path().join("projects"))
                .unwrap()
                .count(),
            0
        );
        f.close().await;
    }

    #[tokio::test]
    async fn recovery_reports_details_and_preserves_existing_work_for_both_choices() {
        let mut f = Fixture::new().await;
        let reviewed = f.review().await;
        f.source.proceed.add_permits(1);
        let first = f.complete(&reviewed.operation_id).await;
        assert_eq!(first.code, Code::Prepared);
        let record = f.record(&first.operation_id);
        let path = std::path::Path::new(&record.manifest.as_ref().unwrap().binding.path);
        f.git(path, &["checkout", "-b", "feature/keep"]);
        std::fs::write(path.join("work.txt"), "staged work").unwrap();
        f.git(path, &["add", "work.txt"]);
        std::fs::write(path.join("work.txt"), "original\n").unwrap();
        std::fs::write(path.join("new.txt"), "new work").unwrap();
        std::fs::write(path.join("POLICY.md"), "locally edited policy").unwrap();
        let index = std::fs::read(path.join(".git/index")).unwrap();
        let queue: QueueView = f
            .hub
            .read(|r| r.require("queue", first.project_id.unwrap()))
            .unwrap();
        rewrite_handoff(&mut f.input, |h| h["releaseId"] = serde_json::json!("two"));
        let second = f.review().await;
        let conflict = f.prepare(&second.operation_id).await;
        assert_eq!(conflict.code, Code::WorkspaceConflict);
        let detail = conflict.conflict.unwrap();
        assert_eq!(detail.changed, Some(2));
        assert_eq!(detail.untracked, Some(1));
        assert_eq!(detail.branch.as_deref(), Some("feature/keep"));
        assert_eq!(
            detail.revision.as_deref(),
            Some(approval(&f.input, seconds(&f.hub)).commit.as_str())
        );
        assert_eq!(detail.project_id, first.project_id);
        let reopened = f
            .complete_choice(
                &second.operation_id,
                Some(ContributionWorkspaceChoice::UseExisting {
                    project_id: first.project_id.unwrap(),
                }),
            )
            .await;
        assert_eq!(reopened.code, Code::Reopened, "{reopened:?}");
        assert_eq!(reopened.project_id, first.project_id);
        assert_eq!(reopened.context.unwrap().release_id, "one");
        assert!(!reopened.execution_authorized);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        f.source.proceed.add_permits(1);
        let separate = f
            .complete_choice(
                &second.operation_id,
                Some(ContributionWorkspaceChoice::CreateSeparate),
            )
            .await;
        assert_eq!(separate.code, Code::Prepared, "{separate:?}");
        assert_ne!(separate.project_id, first.project_id);
        assert_ne!(separate.operation_id, second.operation_id);
        assert_eq!(separate.context.unwrap().release_id, "two");
        let sibling = f.record(&separate.operation_id);
        assert_eq!(
            std::path::Path::new(&sibling.workspace.unwrap().path).parent(),
            path.parent()
        );
        assert_eq!(std::fs::read(path.join(".git/index")).unwrap(), index);
        assert_eq!(
            std::fs::read_to_string(path.join("new.txt")).unwrap(),
            "new work"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("POLICY.md")).unwrap(),
            "locally edited policy"
        );
        assert_eq!(
            f.hub
                .read(|r| r.require::<QueueView>("queue", first.project_id.unwrap()))
                .unwrap(),
            queue
        );
        let again = f
            .complete_choice(
                &second.operation_id,
                Some(ContributionWorkspaceChoice::CreateSeparate),
            )
            .await;
        assert_eq!(again.project_id, separate.project_id);
        assert_eq!(again.code, Code::Reopened);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 2);
        f.close().await;
    }
    #[tokio::test]
    async fn recovery_choices_recheck_approval_project_and_root() {
        let f = Fixture::new().await;
        let reviewed = f.review().await;
        f.source.proceed.add_permits(1);
        let done = f.complete(&reviewed.operation_id).await;
        let refused = f
            .complete_choice(
                &done.operation_id,
                Some(ContributionWorkspaceChoice::UseExisting {
                    project_id: ProjectId::new(),
                }),
            )
            .await;
        assert_eq!(refused.code, Code::WorkspaceConflict);
        let mut untrusted = approval(&f.input, seconds(&f.hub));
        untrusted.packet_sha256 = "0".repeat(64);
        for choice in [
            ContributionWorkspaceChoice::UseExisting {
                project_id: done.project_id.unwrap(),
            },
            ContributionWorkspaceChoice::CreateSeparate,
        ] {
            let refused = f
                .submit(Command::PrepareContribution {
                    operation_id: done.operation_id.clone(),
                    approval: Box::new(untrusted.clone()),
                    workspace_choice: Some(choice),
                })
                .await;
            assert_eq!(refused.code, Code::PacketUntrusted);
        }
        f.hub
            .write(|tx| {
                let mut roots: Vec<LibraryRoot> = tx.read().require("meta", "library_roots")?;
                roots[0].only = Some(vec!["notes".into()]);
                tx.put("meta", "library_roots", &roots)
            })
            .unwrap();
        for choice in [
            ContributionWorkspaceChoice::UseExisting {
                project_id: done.project_id.unwrap(),
            },
            ContributionWorkspaceChoice::CreateSeparate,
        ] {
            let refused = f.complete_choice(&done.operation_id, Some(choice)).await;
            assert_eq!(refused.code, Code::RootChanged);
        }
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        f.close().await;
    }
    #[tokio::test]
    async fn recovery_rejects_a_previous_workspace_after_its_root_is_repointed() {
        for repoint in [true, false] {
            for changed_release in [false, true] {
                let mut f = Fixture::new().await;
                let reviewed = f.review().await;
                f.source.proceed.add_permits(1);
                let first = f.complete(&reviewed.operation_id).await;
                assert_eq!(first.code, Code::Prepared);
                let previous = f.record(&first.operation_id);
                let path = std::path::Path::new(&previous.manifest.as_ref().unwrap().binding.path);
                std::fs::write(path.join("work.txt"), "keep my work").unwrap();
                let queue: QueueView = f
                    .hub
                    .read(|r| r.require("queue", first.project_id.unwrap()))
                    .unwrap();
                let new_root = if repoint {
                    let new_root = f.temp.path().join("new-projects");
                    std::fs::create_dir(&new_root).unwrap();
                    new_root
                } else {
                    // Keep the workspace inode and path intact but replace its parent
                    // root, so checking only the workspace would still allow reuse.
                    let root = f.temp.path().join("projects");
                    let retired = f.temp.path().join("retired-projects");
                    std::fs::rename(&root, &retired).unwrap();
                    std::fs::create_dir(&root).unwrap();
                    std::fs::rename(retired.join(path.file_name().unwrap()), path).unwrap();
                    root
                };
                f.hub
                    .write(|tx| {
                        let mut roots: Vec<LibraryRoot> =
                            tx.read().require("meta", "library_roots")?;
                        roots[0].host_path = new_root.to_str().unwrap().into();
                        tx.put("meta", "library_roots", &roots)
                    })
                    .unwrap();
                // A new review binds the same root ID at its newly authorized location.
                rewrite_handoff(&mut f.input, |h| {
                    if changed_release {
                        h["releaseId"] = serde_json::json!("two");
                    }
                });
                let fresh = f.review().await;
                assert_eq!(fresh.code, Code::AwaitingLocalApproval);
                assert_ne!(fresh.operation_id, first.operation_id);
                for choice in [
                    Some(ContributionWorkspaceChoice::UseExisting {
                        project_id: first.project_id.unwrap(),
                    }),
                    None,
                ] {
                    let refused = f.complete_choice(&fresh.operation_id, choice).await;
                    assert_eq!(refused.code, Code::RootChanged, "{refused:?}");
                    assert!(refused.project_id.is_none());
                    assert!(refused.conflict.is_none());
                }
                let detail = f
                    .hub
                    .read(|r| Ok(f.hub.contribution_conflict(r, &previous)))
                    .unwrap();
                assert!(detail.project_id.is_none());
                assert!(detail.changed.is_none());
                assert!(detail.untracked.is_none());
                assert!(detail.branch.is_none());
                assert!(detail.revision.is_none());
                assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
                // Creating new work in the currently authorized root is still valid.
                f.source.proceed.add_permits(1);
                let separate = f
                    .complete_choice(
                        &fresh.operation_id,
                        Some(ContributionWorkspaceChoice::CreateSeparate),
                    )
                    .await;
                assert_eq!(separate.code, Code::Prepared);
                assert_ne!(separate.project_id, first.project_id);
                let separate = f.record(&separate.operation_id);
                assert_eq!(
                    std::path::Path::new(&separate.workspace.unwrap().path).parent(),
                    Some(new_root.as_path())
                );
                assert_eq!(
                    std::fs::read_to_string(path.join("work.txt")).unwrap(),
                    "keep my work"
                );
                assert_eq!(
                    f.hub
                        .read(|r| r.require::<QueueView>("queue", first.project_id.unwrap()))
                        .unwrap(),
                    queue
                );
                f.close().await;
            }
        }
    }
    #[tokio::test]
    async fn live_progress_trusted_name_and_selected_root_are_bounded() {
        let f = Fixture::new().await;
        let capabilities = f.hub.contribution_capabilities();
        assert!(capabilities.supported);
        assert!(capabilities.unsupported_reason.is_none());
        assert_eq!(capabilities.root_ids, vec![f.input.root_id]);
        assert_eq!(capabilities.contributions_root_id, None);
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::SetContributionRoot {
                    root_id: f.input.root_id,
                }),
            )
            .await;
        assert!(receipt.error.is_none(), "{receipt:?}");
        assert_eq!(
            f.hub.contribution_capabilities().contributions_root_id,
            Some(f.input.root_id)
        );
        let reviewed = f.review().await;
        assert!(reviewed.context.unwrap().display_name.is_none());
        let mut approved = approval(&f.input, seconds(&f.hub));
        approved.display_name = Some("Fixture Game".into());
        let admitted = f
            .submit(Command::PrepareContribution {
                operation_id: reviewed.operation_id.clone(),
                approval: Box::new(approved),
                workspace_choice: None,
            })
            .await;
        assert_eq!(admitted.received_bytes, Some(0));
        assert_eq!(
            admitted.context.unwrap().display_name.as_deref(),
            Some("Fixture Game")
        );
        f.source.entered.acquire().await.unwrap().forget();
        let live = f
            .hub
            .contribution_view(&Caller::local_owner(), &reviewed.operation_id)
            .unwrap();
        assert_eq!(live.received_bytes, Some(f.source.bytes.len() as u64));
        let canceled = f
            .submit(Command::CancelContribution {
                operation_id: reviewed.operation_id,
            })
            .await;
        assert!(canceled.received_bytes.is_none());
        f.source.proceed.add_permits(1);
        f.hub
            .write(|tx| {
                let mut roots: Vec<LibraryRoot> = tx.read().require("meta", "library_roots")?;
                roots[0].writable = false;
                tx.put("meta", "library_roots", &roots)
            })
            .unwrap();
        let capabilities = f.hub.contribution_capabilities();
        assert!(capabilities.root_ids.is_empty());
        assert!(capabilities.contributions_root_id.is_none());
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::SetContributionRoot {
                    root_id: f.input.root_id,
                }),
            )
            .await;
        assert!(receipt.error.is_some());
        f.close().await;
    }
    #[tokio::test]
    async fn unsupported_requirements_are_named_without_raw_packet_values() {
        let mut f = Fixture::new().await;
        rewrite_handoff(&mut f.input, |h| {
            h["compatibility"]["requiredCapabilities"] =
                serde_json::json!(["pinned-import", "task-exchange-draft-1"]);
            h["compatibility"]["protocolVersion"] =
                serde_json::json!("/private/unsupported-protocol");
        });
        let view = f.review().await;
        assert_eq!(view.code, Code::CompatibilityUnsupported);
        assert_eq!(
            view.missing_capabilities,
            vec!["import-protocol-version", "task-exchange-draft-1"]
        );
        assert!(!serde_json::to_string(&view).unwrap().contains("/private/"));
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 0);
        f.close().await;
    }
    #[tokio::test]
    async fn transport_registers_once_and_preserves_dirty_work_queue_and_staging() {
        let f = Fixture::new().await;
        let reviewed = f.review().await;
        assert_eq!(reviewed.code, Code::AwaitingLocalApproval);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 0);
        f.source.proceed.add_permits(1);
        let first = f.complete(&reviewed.operation_id).await;
        assert_eq!(first.code, Code::Prepared, "{first:?}");
        let record = f.record(&first.operation_id);
        let path = &record.manifest.as_ref().unwrap().binding.path;
        assert!(!first.execution_authorized);
        let queue: QueueView = f
            .hub
            .read(|r| r.require("queue", first.project_id.unwrap()))
            .unwrap();
        assert!(queue.paused);
        // Verify that an actual Git client recognizes the materialized commit/index.
        let out = std::process::Command::new("git")
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", f.temp.path())
            .args(["--no-optional-locks", "-C", path, "status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            std::path::Path::new(path).join("setup.sh"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(f.complete(&first.operation_id).await.dirty, Some(true));
        std::fs::write(std::path::Path::new(path).join("work.txt"), "dirty work\n").unwrap();
        std::fs::write(std::path::Path::new(path).join("untracked.txt"), "keep me").unwrap();
        f.hub
            .write(|tx| {
                let mut q: QueueView = tx.read().require("queue", first.project_id.unwrap())?;
                q.paused = false;
                q.revision = 7;
                tx.put("queue", q.project_id, &q)
            })
            .unwrap();
        let reopened = f.complete(&first.operation_id).await;
        assert_eq!(reopened.code, Code::Reopened, "{reopened:?}");
        assert_eq!(reopened.project_id, first.project_id);
        assert_eq!(reopened.dirty, Some(true));
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(path).join("work.txt")).unwrap(),
            "dirty work\n"
        );
        let q: QueueView = f
            .hub
            .read(|r| r.require("queue", first.project_id.unwrap()))
            .unwrap();
        assert!(!q.paused);
        assert_eq!(q.revision, 7);
        std::fs::write(
            std::path::Path::new(path).join("POLICY.md"),
            "changed policy",
        )
        .unwrap();
        assert_eq!(
            f.complete(&first.operation_id).await.code,
            Code::PolicyMismatch
        );
        f.close().await;
    }
    #[tokio::test]
    async fn review_retry_binds_a_root_only_after_the_packet_becomes_valid() {
        let mut f = Fixture::new().await;
        let mut packet: serde_json::Value = serde_json::from_str(&f.input.packet_json).unwrap();
        packet["issuedAt"] = serde_json::json!(seconds(&f.hub) + 10);
        f.input.packet_json = packet.to_string();
        let first = f.review().await;
        assert_eq!(first.code, Code::NotYetValid);
        assert!(f.record(&first.operation_id).root.is_none());
        f.clock.advance(Duration::from_secs(10));
        let retry = f.review().await;
        assert_eq!(retry.operation_id, first.operation_id);
        assert_eq!(retry.code, Code::AwaitingLocalApproval);
        assert!(f.record(&first.operation_id).root.is_some());
        f.source.proceed.add_permits(1);
        assert_eq!(f.complete(&first.operation_id).await.code, Code::Prepared);
        f.close().await;
    }
    #[tokio::test]
    async fn replaced_file_is_dirty_on_reopen_and_blocks_interrupted_registration() {
        for recover in [false, true] {
            let f = Fixture::new().await;
            let first = f.review().await;
            f.source.proceed.add_permits(1);
            let done = f.complete(&first.operation_id).await;
            assert_eq!(done.code, Code::Prepared);
            let manifest = f.record(&done.operation_id).manifest.unwrap();
            let path = std::path::Path::new(&manifest.binding.path).join("work.txt");
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            let f = if recover {
                f.restart_before_registration(&done).await
            } else {
                f
            };
            let reopened = f.complete(&done.operation_id).await;
            if recover {
                assert_eq!(reopened.code, Code::ReconcileRequired);
                assert!(reopened.project_id.is_none());
                assert!(
                    f.hub
                        .read(|r| r.list::<Project>("project"))
                        .unwrap()
                        .is_empty()
                );
            } else {
                assert_eq!(reopened.code, Code::Reopened);
                assert_eq!(reopened.dirty, Some(true));
            }
            assert!(path.is_dir());
            assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
            f.close().await;
        }
    }
    #[tokio::test]
    async fn ordinary_git_status_preserves_clean_reopen_and_recovery() {
        for recover in [false, true] {
            let f = Fixture::new().await;
            let first = f.review().await;
            f.source.proceed.add_permits(1);
            let done = f.complete(&first.operation_id).await;
            assert_eq!(done.code, Code::Prepared);
            let manifest = f.record(&done.operation_id).manifest.unwrap();
            let path = std::path::Path::new(&manifest.binding.path);
            let before = std::fs::read(path.join(".git/index")).unwrap();
            let status = f.git(path, &["status", "--porcelain"]);
            assert!(status.stdout.is_empty(), "{status:?}");
            assert_ne!(before, std::fs::read(path.join(".git/index")).unwrap());
            for version in ["--index-version=3", "--index-version=4"] {
                f.git(path, &["update-index", version]);
                let reopened = f.complete(&done.operation_id).await;
                assert_eq!(reopened.code, Code::Reopened);
                assert_eq!(reopened.dirty, Some(false));
            }
            let f = if recover {
                f.restart_before_registration(&done).await
            } else {
                f
            };
            let reopened = f.complete(&done.operation_id).await;
            assert_eq!(
                reopened.code,
                if recover {
                    Code::Prepared
                } else {
                    Code::Reopened
                }
            );
            assert_eq!(reopened.dirty, Some(false));
            assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
            f.close().await;
        }
    }
    #[tokio::test]
    async fn staged_changes_survive_reopen_and_block_interrupted_registration() {
        for change in ["content", "mode", "deletion"] {
            let f = Fixture::new().await;
            let first = f.review().await;
            f.source.proceed.add_permits(1);
            let done = f.complete(&first.operation_id).await;
            assert_eq!(done.code, Code::Prepared);
            let manifest = f.record(&done.operation_id).manifest.unwrap();
            let path = std::path::Path::new(&manifest.binding.path);
            match change {
                "content" => {
                    std::fs::write(path.join("work.txt"), "staged work\n").unwrap();
                    f.git(path, &["add", "--", "work.txt"]);
                    std::fs::write(path.join("work.txt"), "original\n").unwrap();
                }
                "mode" => {
                    f.git(path, &["update-index", "--chmod=+x", "--", "work.txt"]);
                }
                "deletion" => {
                    f.git(path, &["update-index", "--force-remove", "--", "work.txt"]);
                }
                _ => unreachable!(),
            }
            let staged = std::fs::read(path.join(".git/index")).unwrap();
            let reopened = f.complete(&done.operation_id).await;
            assert_eq!(reopened.code, Code::Reopened, "{change}");
            assert_eq!(reopened.dirty, Some(true), "{change}");
            let f = f.restart_before_registration(&done).await;
            let refused = f.complete(&done.operation_id).await;
            assert_eq!(refused.code, Code::ReconcileRequired, "{change}");
            assert!(refused.project_id.is_none());
            assert_eq!(staged, std::fs::read(path.join(".git/index")).unwrap());
            assert_eq!(std::fs::read(path.join("work.txt")).unwrap(), b"original\n");
            assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
            f.close().await;
        }
    }
    #[tokio::test]
    async fn index_parser_bounds_and_required_extensions_fail_closed() {
        let f = Fixture::new().await;
        let first = f.review().await;
        f.source.proceed.add_permits(1);
        let done = f.complete(&first.operation_id).await;
        assert_eq!(done.code, Code::Prepared);
        let manifest = f.record(&done.operation_id).manifest.unwrap();
        let path = std::path::Path::new(&manifest.binding.path);
        let original = std::fs::read(path.join(".git/index")).unwrap();
        let sizes = manifest
            .files
            .keys()
            .map(|p| {
                (
                    p.clone(),
                    std::fs::metadata(path.join(p)).unwrap().len() as u32,
                )
            })
            .collect();
        let unchanged =
            |bytes: &[u8]| index::unchanged(bytes, &manifest.index_hash, &sizes, &|| Ok(()));
        assert_eq!(unchanged(&original), Ok(true));
        for end in 0..original.len() {
            assert_ne!(unchanged(&original[..end]), Ok(true), "truncated at {end}");
        }
        let resign = |bytes: &mut Vec<u8>| {
            bytes.extend_from_slice(&pack::sha1(&[bytes], &|| Ok(())).unwrap());
        };
        for signature in [b"TEST", b"link", b"sdir"] {
            let mut bytes = original[..original.len() - 20].to_vec();
            bytes.extend_from_slice(signature);
            bytes.extend_from_slice(&5u32.to_be_bytes());
            bytes.extend_from_slice(b"cache");
            resign(&mut bytes);
            assert_eq!(unchanged(&bytes), Ok(signature == b"TEST"));
        }
        // Racy stat sizes, timestamps and assume-valid never conceal a file
        // change: the caller hashes every tracked file before normalization.
        let mut bytes = original[..original.len() - 20].to_vec();
        bytes[12..36].fill(0xff);
        bytes[48..52].fill(0);
        bytes[72] |= 0x80;
        resign(&mut bytes);
        assert_eq!(unchanged(&bytes), Ok(true));
        for (offset, value) in [(7, 5), (12 + 60, 0x10), (12 + 40, 0xff), (12 + 62, 0)] {
            let mut bytes = original[..original.len() - 20].to_vec();
            bytes[offset] = value;
            resign(&mut bytes);
            assert_ne!(unchanged(&bytes), Ok(true));
        }
        // Even a rechecksummed truncated index must never panic or pass.
        for end in 0..original.len() - 20 {
            let mut bytes = original[..end].to_vec();
            resign(&mut bytes);
            assert_ne!(
                unchanged(&bytes),
                Ok(true),
                "rechecksummed truncation at {end}"
            );
        }
        let mut version_three = original[..original.len() - 20].to_vec();
        version_three[7] = 3;
        resign(&mut version_three);
        assert_eq!(unchanged(&version_three), Ok(true));
        f.git(path, &["update-index", "--index-version=4"]);
        let compressed = std::fs::read(path.join(".git/index")).unwrap();
        assert_eq!(compressed[7], 4);
        assert_eq!(unchanged(&compressed), Ok(true));
        for end in 0..compressed.len() - 20 {
            let mut bytes = compressed[..end].to_vec();
            resign(&mut bytes);
            assert_ne!(
                unchanged(&bytes),
                Ok(true),
                "compressed truncation at {end}"
            );
        }
        for prefix in [vec![1], vec![0xff; 16]] {
            let mut bytes = compressed[..compressed.len() - 20].to_vec();
            bytes.splice(74..75, prefix);
            resign(&mut bytes);
            assert_eq!(unchanged(&bytes), Err(Code::WorkspaceConflict));
        }
        assert_eq!(
            index::unchanged(&original, &manifest.index_hash, &sizes, &|| Err(
                Code::Canceled
            )),
            Err(Code::Canceled)
        );
        f.close().await;
    }
    #[tokio::test]
    async fn registration_respects_snapshot_budget_and_recovers_without_refetch() {
        let mut f = Fixture::new().await;
        let first = f.review().await;
        f.source.proceed.add_permits(1);
        let done = f.complete(&first.operation_id).await;
        assert_eq!(done.code, Code::Prepared);
        let existing = done.project_id.unwrap();
        // Fill the existing snapshot up to its supported budget before opening
        // a distinct repository. The pending registration must roll back whole.
        f.hub
            .write(|tx| {
                let snapshot =
                    crate::history::always(&tx.read(), &protocol::auth::Grants::owner())?;
                let mut project: Project = tx.read().require("project", existing)?;
                project
                    .name
                    .push_str(&"x".repeat(768 * 1024 - crate::history::size(&snapshot) - 32));
                tx.put("project", existing, &project)?;
                crate::history::admit_projects(&tx.read())
            })
            .unwrap();
        let mut handoff: serde_json::Value = serde_json::from_str(&f.input.handoff_json).unwrap();
        handoff["source"]["repositoryUrl"] =
            serde_json::json!("https://source.example/owned/second");
        f.input.handoff_json = handoff.to_string();
        let mut packet: serde_json::Value = serde_json::from_str(&f.input.packet_json).unwrap();
        packet["packetId"] = serde_json::json!("second");
        packet["handoffSha256"] = serde_json::json!(validate::digest(&f.input.handoff_json));
        f.input.packet_json = packet.to_string();
        let second = f.review().await;
        f.source.proceed.add_permits(1);
        let refused = f.complete(&second.operation_id).await;
        assert_eq!(refused.code, Code::ReconcileRequired);
        assert!(refused.project_id.is_none());
        f.hub
            .read(|r| {
                assert_eq!(r.list::<Project>("project")?.len(), 1);
                assert_eq!(r.list::<QueueView>("queue")?.len(), 1);
                crate::history::admit_projects(r)
            })
            .unwrap();
        let manifest = f.record(&second.operation_id).manifest.unwrap();
        assert!(
            std::path::Path::new(&manifest.binding.path)
                .join("work.txt")
                .is_file()
        );
        f.hub
            .write(|tx| {
                let mut project: Project = tx.read().require("project", existing)?;
                project.name = "fixture".into();
                tx.put("project", existing, &project)
            })
            .unwrap();
        let retried = f.complete(&second.operation_id).await;
        assert_eq!(retried.code, Code::Prepared);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 2);
        f.close().await;
    }
    #[tokio::test]
    async fn concurrent_packet_ids_converge_and_cancel_stops_acquisition() {
        let mut f = Fixture::new().await;
        let first = f.review().await;
        let running = f.prepare(&first.operation_id).await;
        f.source.entered.acquire().await.unwrap().forget();
        let mut p: serde_json::Value = serde_json::from_str(&f.input.packet_json).unwrap();
        p["packetId"] = serde_json::json!("second");
        f.input.packet_json = p.to_string();
        let second = f.review().await;
        let duplicate = f.prepare(&second.operation_id).await;
        assert_eq!(duplicate.operation_id, running.operation_id);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        let mut sub = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
        assert_eq!(
            f.submit(Command::CancelContribution {
                operation_id: running.operation_id.clone()
            })
            .await
            .code,
            Code::Canceled
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !f.record(&running.operation_id).active {
                    break;
                }
                sub.live.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(
            f.hub
                .read(|r| r.list::<Project>("project"))
                .unwrap()
                .is_empty()
        );
        assert!(
            f.temp
                .path()
                .join("projects")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
        f.source.proceed.add_permits(1);
        let resumed = f.complete(&second.operation_id).await;
        assert_eq!(resumed.code, Code::Prepared, "{resumed:?}");
        assert_eq!(resumed.operation_id, first.operation_id);
        f.close().await;
    }
    #[tokio::test]
    async fn changed_envelopes_roots_expiry_and_grants_block_without_fetching() {
        let mut f = Fixture::new().await;
        let mut other_host = f.input.clone();
        other_host.host_id = HostId::new();
        assert_eq!(
            f.submit(Command::ReviewContribution {
                input: Box::new(other_host)
            })
            .await
            .code,
            Code::HostUnavailable
        );
        let first = f.review().await;
        let mut bad = approval(&f.input, seconds(&f.hub));
        bad.packet_sha256 = "0".repeat(64);
        let blocked = f
            .submit(Command::PrepareContribution {
                workspace_choice: None,
                operation_id: first.operation_id.clone(),
                approval: Box::new(bad),
            })
            .await;
        assert_eq!(blocked.code, Code::PacketUntrusted);
        f.clock.advance(Duration::from_secs(3600));
        assert_eq!(f.review().await.code, Code::Expired);
        assert_eq!(f.prepare(&first.operation_id).await.code, Code::Expired);
        let mut input = f.input.clone();
        input.host_id = HostId::new();
        assert_eq!(
            f.submit(Command::ReviewContribution {
                input: Box::new(input)
            })
            .await
            .code,
            Code::Expired
        );
        // A distinct fresh packet can be reviewed but a changed root invalidates it.
        let p = fixture(&[]);
        f.input = input_for(&f, &p);
        let reviewed = f.review().await;
        std::fs::rename(
            f.temp.path().join("projects"),
            f.temp.path().join("old-projects"),
        )
        .unwrap();
        std::fs::create_dir(f.temp.path().join("projects")).unwrap();
        assert_eq!(f.review().await.code, Code::RootChanged);
        assert_eq!(
            f.prepare(&reviewed.operation_id).await.code,
            Code::RootChanged
        );
        let mut caller = Caller::local_owner();
        caller.grants.admin = false;
        let receipt = f
            .hub
            .submit(
                &caller,
                CommandEnvelope::new(Command::ReviewContribution {
                    input: Box::new(f.input.clone()),
                }),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::PermissionDenied);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 0);
        f.close().await;
    }

    #[tokio::test]
    async fn changed_packet_id_binding_and_source_revision_never_replace_a_workspace() {
        let mut f = Fixture::new().await;
        let first = f.review().await;
        f.source.proceed.add_permits(1);
        assert_eq!(f.complete(&first.operation_id).await.code, Code::Prepared);
        let mut p: serde_json::Value = serde_json::from_str(&f.input.packet_json).unwrap();
        p["expiresAt"] = serde_json::json!(seconds(&f.hub) + 3500);
        f.input.packet_json = p.to_string();
        let changed = f.review().await;
        assert_eq!(
            f.prepare(&changed.operation_id).await.code,
            Code::IdConflict
        );
        p["packetId"] = serde_json::json!("new-id");
        let mut h: serde_json::Value = serde_json::from_str(&f.input.handoff_json).unwrap();
        h["source"]["commit"] = serde_json::json!("b".repeat(40));
        f.input.handoff_json = h.to_string();
        p["handoffSha256"] = serde_json::json!(validate::digest(&f.input.handoff_json));
        f.input.packet_json = p.to_string();
        let changed = f.review().await;
        assert_eq!(
            f.prepare(&changed.operation_id).await.code,
            Code::WorkspaceConflict
        );
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.hub.read(|r| r.list::<Project>("project")).unwrap().len(),
            1
        );
        f.close().await;
    }
    #[tokio::test]
    async fn restart_reconciles_a_fully_written_unregistered_checkout_without_refetch() {
        let f = Fixture::new().await;
        let first = f.review().await;
        f.source.proceed.add_permits(1);
        let done = f.complete(&first.operation_id).await;
        assert_eq!(done.code, Code::Prepared);
        let f = f.restart_before_registration(&done).await;
        assert_eq!(
            f.record(&done.operation_id).view.code,
            Code::ReconcileRequired
        );
        let recovered = f.complete(&done.operation_id).await;
        assert_eq!(recovered.code, Code::Prepared, "{recovered:?}");
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.hub.read(|r| r.list::<Project>("project")).unwrap().len(),
            1
        );
        f.close().await;
    }
    #[tokio::test]
    async fn symlinked_policy_and_workspace_replacement_preserve_external_bytes() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new().await;
        let first = f.review().await;
        f.source.proceed.add_permits(1);
        assert_eq!(f.complete(&first.operation_id).await.code, Code::Prepared);
        let record = f.record(&first.operation_id);
        let workspace = std::path::PathBuf::from(record.manifest.unwrap().binding.path);
        let outside = f.temp.path().join("external");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("POLICY.md"), "private bytes").unwrap();
        std::fs::remove_file(workspace.join("POLICY.md")).unwrap();
        symlink(outside.join("POLICY.md"), workspace.join("POLICY.md")).unwrap();
        assert_eq!(
            f.complete(&first.operation_id).await.code,
            Code::PolicyMismatch
        );
        std::fs::rename(&workspace, workspace.with_extension("saved")).unwrap();
        symlink(&outside, &workspace).unwrap();
        assert_eq!(
            f.complete(&first.operation_id).await.code,
            Code::WorkspaceConflict
        );
        assert_eq!(
            std::fs::read_to_string(outside.join("POLICY.md")).unwrap(),
            "private bytes"
        );
        assert_eq!(outside.read_dir().unwrap().count(), 1);
        f.close().await;
    }
    #[tokio::test]
    async fn expired_dispatch_releases_the_reservation_and_never_acquires() {
        let f = Fixture::new().await;
        let reviewed = f.review().await;
        let command = CommandId::new();
        let (result, pending) = f
            .hub
            .write(|tx| {
                f.hub.prepare_contribution(
                    tx,
                    &Caller::local_owner(),
                    &reviewed.operation_id,
                    &approval(&f.input, seconds(&f.hub)),
                    None,
                    command,
                )
            })
            .unwrap();
        assert!(pending);
        assert!(matches!(result, CommandResult::Contribution { .. }));
        let result = f
            .hub
            .execute_contribution(
                &Caller::local_owner(),
                &reviewed.operation_id,
                command,
                Some(f.hub.inner.clock.now()),
            )
            .await
            .unwrap();
        let CommandResult::Contribution { operation } = result else {
            panic!()
        };
        assert_eq!(operation.code, Code::Expired);
        assert!(!f.record(&reviewed.operation_id).active);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 0);
        f.close().await;
    }

    #[tokio::test]
    async fn fresh_resume_preserves_an_incomplete_attempt_and_cannot_exceed_two_workers() {
        let mut f = Fixture::new().await;
        let first = f.review().await;
        let mut rec = f.record(&first.operation_id);
        let prior = workspace::create(
            rec.root.as_ref().unwrap(),
            &format!(".ophio-contribution-{}-1", first.operation_id),
        )
        .unwrap();
        std::fs::write(
            std::path::Path::new(&prior.path).join("untracked-note"),
            "survives interruption",
        )
        .unwrap();
        rec.workspace = Some(prior.clone());
        rec.attempts = 1;
        rec.view.code = Code::ReconcileRequired;
        f.hub.write(|tx| save(tx, &rec)).unwrap();
        let running = f.prepare(&first.operation_id).await;
        assert_eq!(running.code, Code::Acquiring);
        f.source.entered.acquire().await.unwrap().forget();
        let original = f.input.clone();
        for n in 2..=3 {
            let mut h: serde_json::Value = serde_json::from_str(&original.handoff_json).unwrap();
            h["source"]["repositoryUrl"] =
                serde_json::json!(format!("https://source.example/owned/fixture{n}"));
            f.input.handoff_json = h.to_string();
            let mut p: serde_json::Value = serde_json::from_str(&original.packet_json).unwrap();
            p["packetId"] = serde_json::json!(format!("packet{n}"));
            p["handoffSha256"] = serde_json::json!(validate::digest(&f.input.handoff_json));
            f.input.packet_json = p.to_string();
            let reviewed = f.review().await;
            let result = f.prepare(&reviewed.operation_id).await;
            assert_eq!(
                result.code,
                if n == 2 { Code::Acquiring } else { Code::Busy }
            );
            if n == 2 {
                f.source.entered.acquire().await.unwrap().forget();
            }
        }
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 2);
        // Advancing past the approval window makes both in-flight fetches stop,
        // even when the source has not returned any bytes.
        f.clock.advance(Duration::from_secs(301));
        let mut sub = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while f
                .hub
                .read(|r| Ok(r.list::<Record>("contribution")?.iter().any(|r| r.active)))
                .unwrap()
            {
                sub.live.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(
            f.hub
                .read(|r| r.list::<Project>("project"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&prior.path).join("untracked-note"))
                .unwrap(),
            "survives interruption"
        );
        f.input = original;
        f.source.proceed.add_permits(1);
        let resumed = f.complete(&first.operation_id).await;
        assert_eq!(resumed.code, Code::Prepared, "{resumed:?}");
        assert_ne!(
            f.record(&first.operation_id).manifest.unwrap().binding.path,
            prior.path
        );
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&prior.path).join("untracked-note"))
                .unwrap(),
            "survives interruption"
        );
        f.close().await;
    }

    #[tokio::test]
    async fn insufficient_disk_blocks_admission_and_a_drop_during_acquisition() {
        #[derive(Debug)]
        struct Space(std::sync::atomic::AtomicU64);
        impl workspace::Space for Space {
            fn available_bytes(&self, _: &workspace::Binding) -> Result<u128, Code> {
                Ok(self.0.load(Ordering::SeqCst) as u128)
            }
        }
        let f = Fixture::new().await;
        let reviewed = f.review().await;
        let space = Arc::new(Space(std::sync::atomic::AtomicU64::new(1)));
        *f.hub.inner.contributions.space.lock().unwrap() = space.clone();
        let refused = f.prepare(&reviewed.operation_id).await;
        assert_eq!(refused.code, Code::DiskFull);
        assert_eq!(
            refused.space,
            Some(ContributionSpace {
                needed_bytes: 2 * 1024 * 1024 * 1024,
                available_bytes: 1
            })
        );
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 0);
        space.0.store(4 * 1024 * 1024 * 1024, Ordering::SeqCst);
        let mut sub = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
        assert_eq!(
            f.prepare(&reviewed.operation_id).await.code,
            Code::Acquiring
        );
        f.source.entered.acquire().await.unwrap().forget();
        space.0.store(1, Ordering::SeqCst);
        f.source.proceed.add_permits(1);
        tokio::time::timeout(Duration::from_secs(3), async {
            while f.record(&reviewed.operation_id).active {
                sub.live.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        let refused = f.record(&reviewed.operation_id).view;
        assert_eq!(refused.code, Code::DiskFull);
        assert_eq!(refused.space.unwrap().available_bytes, 1);
        assert!(refused.received_bytes.is_none());
        assert!(
            f.hub
                .read(|r| r.list::<Project>("project"))
                .unwrap()
                .is_empty()
        );
        assert!(
            f.temp
                .path()
                .join("projects")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
        f.close().await;
    }
    #[tokio::test]
    async fn restricted_roots_cannot_authorize_acquisition_or_continue_a_worker() {
        let f = Fixture::new().await;
        let restrict = |only: Option<Vec<String>>| {
            f.hub
                .write(|tx| {
                    let mut roots: Vec<LibraryRoot> = tx.read().require("meta", "library_roots")?;
                    roots[0].only = only;
                    tx.put("meta", "library_roots", &roots)
                })
                .unwrap();
        };
        restrict(Some(vec!["POLICY.md".into()]));
        let refused = f.review().await;
        assert_eq!(refused.code, Code::RootChanged);
        assert_eq!(f.review().await.code, Code::RootChanged);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 0);

        restrict(None);
        let fresh = f.input.clone();
        let reviewed = f
            .submit(Command::ReviewContribution {
                input: Box::new(fresh.clone()),
            })
            .await;
        assert_eq!(reviewed.operation_id, refused.operation_id);
        assert_eq!(reviewed.code, Code::AwaitingLocalApproval);
        // Revoking whole-root access after review must block admission.
        restrict(Some(vec!["POLICY.md".into()]));
        let prepare = || Command::PrepareContribution {
            workspace_choice: None,
            operation_id: reviewed.operation_id.clone(),
            approval: Box::new(approval(&fresh, seconds(&f.hub))),
        };
        assert_eq!(f.submit(prepare()).await.code, Code::RootChanged);
        assert_eq!(f.source.calls.load(Ordering::SeqCst), 0);

        restrict(None);
        assert_eq!(f.submit(prepare()).await.code, Code::Acquiring);
        f.source.entered.acquire().await.unwrap().forget();
        let mut sub = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
        restrict(Some(vec![]));
        tokio::time::timeout(Duration::from_secs(3), async {
            while f.record(&reviewed.operation_id).active {
                sub.live.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            f.record(&reviewed.operation_id).view.code,
            Code::RootChanged
        );
        assert!(
            f.temp
                .path()
                .join("projects")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
        f.close().await;
    }

    fn input_for(f: &Fixture, p: &FixturePack) -> ContributionInput {
        input(
            p,
            f.input.host_id,
            f.input.root_id,
            seconds(&f.hub),
            "fresh",
        )
    }
}

#[test]
fn pack_resolves_both_delta_encodings_and_bounds_expansion() {
    use std::io::Write;
    let f = fixture(&[]);
    let base = b"Owned fixture instructions.\n";
    let base_oid = object(3, base.to_vec()).0;
    let target = [base.as_slice(), b"new"].concat();
    let delta = [
        vec![
            base.len() as u8,
            target.len() as u8,
            0x90,
            base.len() as u8,
            3,
        ],
        b"new".to_vec(),
    ]
    .concat();
    for kind in [6, 7] {
        let mut bytes = f.bytes[..f.bytes.len() - 20].to_vec();
        let count = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        bytes[8..12].copy_from_slice(&(count + 1).to_be_bytes());
        let offset = bytes.len();
        bytes.push((kind << 4) | delta.len() as u8);
        if kind == 7 {
            for i in (0..40).step_by(2) {
                bytes.push(u8::from_str_radix(&base_oid[i..i + 2], 16).unwrap());
            }
        } else {
            let mut n = offset - 12;
            let mut encoded = vec![(n & 127) as u8];
            loop {
                n >>= 7;
                if n == 0 {
                    break;
                }
                n -= 1;
                encoded.push((n & 127) as u8 | 128);
            }
            encoded.reverse();
            bytes.extend(encoded);
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&delta).unwrap();
        bytes.extend(z.finish().unwrap());
        bytes.extend_from_slice(&pack::sha1(&[&bytes], &|| Ok(())).unwrap());
        let tree = pack::decode(&bytes, &f.commit, &|| Ok(())).unwrap();
        assert_eq!(
            tree.objects
                .get(&object(3, target.clone()).0)
                .unwrap()
                .bytes,
            target
        );
    }
    // A declared huge object is refused before decompression or allocation.
    let mut bytes = b"PACK\0\0\0\x02\0\0\0\x01".to_vec();
    let mut n = pack::OBJECT + 1;
    bytes.push(0xb0 | (n as u8 & 15));
    n >>= 4;
    while n != 0 {
        let mut b = n as u8 & 127;
        n >>= 7;
        if n != 0 {
            b |= 128;
        }
        bytes.push(b);
    }
    bytes.extend_from_slice(&pack::sha1(&[&bytes], &|| Ok(())).unwrap());
    assert_eq!(
        pack::decode(&bytes, &f.commit, &|| Ok(())).unwrap_err(),
        Code::LimitExceeded
    );
}

#[tokio::test]
async fn http_profile_reads_real_git_packets_and_limits_streamed_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    fn pkt(data: &[u8]) -> Vec<u8> {
        [format!("{:04x}", data.len() + 4).as_bytes(), data].concat()
    }
    let f = fixture(&[]);
    let pack = f.bytes.clone();
    let commit = f.commit.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for i in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let end = loop {
                let mut byte = [0; 1];
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break request.len();
                }
                assert!(request.len() < 4096);
            };
            let header = String::from_utf8(request.clone()).unwrap();
            assert!(!header.to_ascii_lowercase().contains("authorization:"));
            let length = header
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|s| s.parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            request.resize(end + length, 0);
            stream.read_exact(&mut request[end..]).await.unwrap();
            let (mime, body) = if i == 0 {
                assert!(
                    header.starts_with("GET /owned/fixture/info/refs?service=git-upload-pack ")
                );
                ("advertisement",[pkt(b"# service=git-upload-pack\n"),b"0000".to_vec(),pkt(format!("{commit} refs/heads/main\0shallow side-band-64k ofs-delta no-progress object-format=sha1\n").as_bytes()),b"0000".to_vec()].concat())
            } else {
                assert!(header.starts_with("POST /owned/fixture/git-upload-pack "));
                let wants = std::str::from_utf8(&request[end..]).unwrap();
                assert!(wants.contains(&format!("want {commit} ")));
                assert!(wants.contains("deepen 1\n"));
                assert!(!wants.contains("have "));
                (
                    "result",
                    [
                        pkt(format!("shallow {commit}\n").as_bytes()),
                        b"0000".to_vec(),
                        pkt(b"NAK\n"),
                        pkt(&[vec![1], pack.clone()].concat()),
                        b"0000".to_vec(),
                    ]
                    .concat(),
                )
            };
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-{mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        }
    });
    // Test-only loopback transport. Production Network always builds its own
    // HTTPS client after rejecting and pinning all DNS addresses.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let transferred = std::sync::atomic::AtomicU64::new(0);
    let received = network::transfer(
        &client,
        &format!("http://{address}/owned/fixture"),
        &f.commit,
        &transferred,
    )
    .await
    .unwrap();
    assert_eq!(received, f.bytes);
    let transferred = transferred.load(std::sync::atomic::Ordering::Relaxed);
    assert!(transferred > received.len() as u64);
    assert!(transferred <= network::TRANSFER as u64);
    server.await.unwrap();
    for (headers, body, expected) in [
        (
            "Transfer-Encoding: chunked\r\n",
            "4\r\ndata\r\n4\r\nmore\r\n0\r\n\r\n",
            Code::LimitExceeded,
        ),
        ("Content-Length: 99\r\n", "", Code::LimitExceeded),
        (
            "Content-Encoding: gzip\r\nContent-Length: 0\r\n",
            "",
            Code::AcquisitionFailed,
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\n{headers}Connection: close\r\n\r\n{body}")
                        .as_bytes(),
                )
                .await
                .unwrap();
        });
        let response = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            network::body(response, 5, &std::sync::atomic::AtomicU64::new(0))
                .await
                .unwrap_err(),
            expected
        );
        server.await.unwrap();
    }
}
