// SPDX-License-Identifier: Apache-2.0
//! Deterministic, in-memory ports for integration tests. These never touch a
//! real desktop, launch a native tool, or perform inference.
use async_trait::async_trait;
use ports::*;
use protocol::{auth::PairRequest, capability::*, control::*, model::*, time::Timestamp, *};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, broadcast, mpsc};

impl crate::CompanionHub {
    /// Make delivery retries and the next status/feed check due without waiting.
    pub async fn check_reports_now(&self) -> Result<(), ProtocolError> {
        {
            let _one = self.inner.reports.sending.lock().await;
            self.write(|tx| {
                tx.put(store::reports::META, "poll_after", &Timestamp(0))?;
                for mut row in tx
                    .read()
                    .list::<store::reports::ReportDelivery>(store::reports::QUEUE)?
                {
                    row.after = Timestamp(0);
                    tx.put(store::reports::QUEUE, row.delivery.key(), &row)?;
                }
                Ok(())
            })?;
        }
        self.reports_once().await.map(|_| ())
    }
}

#[derive(Debug)]
pub struct FakeClock {
    base: Instant,
    millis: AtomicU64,
    wall: i64,
}
impl Default for FakeClock {
    fn default() -> Self {
        Self {
            base: Instant::now(),
            millis: AtomicU64::new(0),
            wall: 1_700_000_000_000,
        }
    }
}
impl FakeClock {
    pub fn advance(&self, duration: Duration) {
        self.millis
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
    }
}
impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.wall + self.millis.load(Ordering::SeqCst) as i64)
    }
    fn monotonic(&self) -> Instant {
        self.base + Duration::from_millis(self.millis.load(Ordering::SeqCst))
    }
}

pub struct FakeHarness {
    pub starts: AtomicU64,
    pub controls: Arc<FakeRunControl>,
    /// Zero permits holds dispatch before the adapter returns. Add one to ack
    /// the spawn; `SessionStarted` remains an explicit injected event.
    pub spawn_gate: Semaphore,
    senders: Mutex<HashMap<RunId, mpsc::Sender<HarnessEvent>>>,
    pub acknowledge_spawn: AtomicBool,
    pub specs: Mutex<Vec<RunSpec>>,
    /// Refuses the next start as unsupported, before any native write.
    pub refuse_start: Mutex<Option<String>>,
    pub fail_spawn: AtomicBool,
}
impl std::fmt::Debug for FakeHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeHarness").finish_non_exhaustive()
    }
}
impl Default for FakeHarness {
    fn default() -> Self {
        Self {
            starts: AtomicU64::new(0),
            controls: Arc::new(FakeRunControl::default()),
            spawn_gate: Semaphore::new(0),
            senders: Mutex::new(HashMap::new()),
            acknowledge_spawn: AtomicBool::new(true),
            specs: Mutex::new(Vec::new()),
            refuse_start: Mutex::new(None),
            fail_spawn: AtomicBool::new(false),
        }
    }
}
impl FakeHarness {
    pub async fn emit(&self, id: RunId, event: HarnessEvent) {
        let sender = self
            .senders
            .lock()
            .expect("fake mutex")
            .get(&id)
            .cloned()
            .expect("run exists");
        sender.send(event).await.expect("run listener");
    }
    pub fn runs(&self) -> Vec<RunId> {
        self.senders
            .lock()
            .expect("fake mutex")
            .keys()
            .copied()
            .collect()
    }
}
#[async_trait]
impl HarnessAdapter for FakeHarness {
    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }
    async fn probe(&self) -> HarnessInfo {
        let a = Availability::Available;
        HarnessInfo {
            catalog: None,
            usage_limit: None,
            kind: self.kind(),
            display_name: "Synthetic harness".into(),
            native_version: Some("test".into()),
            integration: IntegrationLevel::Managed,
            installed: a.clone(),
            account: AccountReadiness::SignedIn {
                account_label: None,
                plan: None,
            },
            models: Vec::new(),
            efforts: Vec::new(),
            modes: Vec::new(),
            capabilities: HarnessCapabilities {
                cancel_queued_message: None,
                start_run: a.clone(),
                steer_running: a.clone(),
                interrupt: a.clone(),
                native_approvals: a.clone(),
                resume_session: a.clone(),
                child_agents: a.clone(),
                computer_use_delegation: a.clone(),
                model_selection: a.clone(),
                effort_selection: a,
            },
        }
    }
    async fn start_run(&self, spec: RunSpec) -> Result<RunStarted, HarnessError> {
        self.specs.lock().expect("fake mutex").push(spec.clone());
        self.starts.fetch_add(1, Ordering::SeqCst);
        if let Some(reason) = self.refuse_start.lock().expect("fake mutex").take() {
            return Err(HarnessError::Unsupported(reason));
        }
        if self.fail_spawn.load(Ordering::SeqCst) {
            return Err(HarnessError::SpawnFailed("synthetic spawn failure".into()));
        }
        let (sender, events) = mpsc::channel(128);
        self.senders
            .lock()
            .expect("fake mutex")
            .insert(spec.run_id, sender);
        if !self.acknowledge_spawn.load(Ordering::SeqCst) {
            self.spawn_gate
                .acquire()
                .await
                .map_err(|_| HarnessError::NotActive)?
                .forget();
        }
        Ok(RunStarted {
            control: self.controls.clone(),
            events,
        })
    }
}
#[derive(Debug)]
pub struct FakeRunControl {
    pub interrupts: AtomicU64,
    pub messages: AtomicU64,
    pub message_refs: Mutex<Vec<MessageRef>>,
    /// What each message said, in order.
    pub texts: Mutex<Vec<String>>,
    pub resolutions: AtomicU64,
    /// Each decision the agent was given, in order.
    pub decisions: Mutex<Vec<protocol::command::ApprovalDecision>>,
    pub fail_interrupt: AtomicBool,
    pub hold_interrupt: AtomicBool,
    pub terminates: AtomicU64,
    pub private_confirmed: AtomicBool,
    pub delivery: Mutex<DeliveryState>,
    pub hold_message: AtomicBool,
    pub message_entered: Semaphore,
    pub message_gate: Semaphore,
    pub hold_resolution: AtomicBool,
    pub resolution_entered: Semaphore,
    pub resolution_gate: Semaphore,
    /// The adapter refuses a decision before answering the agent.
    pub refuse_resolution: AtomicBool,
    pub containment: Mutex<Option<Containment>>,
    /// Force stop waits at `terminate_gate` once `terminate_entered` says it
    /// arrived, then confirms the end only with `confirm_terminate`.
    pub hold_terminate: AtomicBool,
    pub terminate_entered: Semaphore,
    pub terminate_gate: Semaphore,
    pub confirm_terminate: AtomicBool,
}
impl Default for FakeRunControl {
    fn default() -> Self {
        Self {
            interrupts: AtomicU64::new(0),
            messages: AtomicU64::new(0),
            message_refs: Mutex::new(Vec::new()),
            texts: Mutex::new(Vec::new()),
            resolutions: AtomicU64::new(0),
            decisions: Mutex::new(Vec::new()),
            fail_interrupt: AtomicBool::new(false),
            hold_interrupt: AtomicBool::new(false),
            terminates: AtomicU64::new(0),
            private_confirmed: AtomicBool::new(false),
            delivery: Mutex::new(DeliveryState::NativeDelivered),
            hold_message: AtomicBool::new(false),
            message_entered: Semaphore::new(0),
            message_gate: Semaphore::new(0),
            hold_resolution: AtomicBool::new(false),
            resolution_entered: Semaphore::new(0),
            resolution_gate: Semaphore::new(0),
            refuse_resolution: AtomicBool::new(false),
            containment: Mutex::new(None),
            hold_terminate: AtomicBool::new(false),
            terminate_entered: Semaphore::new(0),
            terminate_gate: Semaphore::new(0),
            confirm_terminate: AtomicBool::new(false),
        }
    }
}
#[async_trait]
impl RunControl for FakeRunControl {
    async fn pause_for_private(&self) -> Result<(), HarnessError> {
        self.interrupt().await?;
        if self.private_confirmed.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(HarnessError::Unsupported(
                "Synthetic process tree is not confirmed paused".into(),
            ))
        }
    }
    async fn send_message(
        &self,
        message_ref: MessageRef,
        input: PromptInput,
        _: protocol::command::MessageDelivery,
    ) -> Result<DeliveryState, HarnessError> {
        self.messages.fetch_add(1, Ordering::SeqCst);
        self.texts.lock().expect("fake mutex").push(input.text);
        self.message_refs
            .lock()
            .expect("fake mutex")
            .push(message_ref);
        self.message_entered.add_permits(1);
        if self.hold_message.load(Ordering::SeqCst) {
            self.message_gate
                .acquire()
                .await
                .expect("message gate")
                .forget();
        }
        Ok(*self.delivery.lock().expect("fake mutex"))
    }
    async fn interrupt(&self) -> Result<(), HarnessError> {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
        if self.hold_interrupt.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.fail_interrupt.load(Ordering::SeqCst) {
            Err(HarnessError::Protocol("Synthetic lost ack".into()))
        } else {
            Ok(())
        }
    }
    async fn terminate(&self) -> Result<(), HarnessError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        self.terminate_entered.add_permits(1);
        if self.hold_terminate.load(Ordering::SeqCst) {
            self.terminate_gate
                .acquire()
                .await
                .expect("terminate gate")
                .forget();
        }
        if self.confirm_terminate.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(HarnessError::Unsupported(
                "Synthetic process tree end is not confirmed".into(),
            ))
        }
    }
    fn close_admission(&self) {}
    fn containment(&self) -> Option<Containment> {
        self.containment.lock().expect("fake mutex").clone()
    }
    async fn resolve_approval(
        &self,
        _: &str,
        decision: &protocol::command::ApprovalDecision,
    ) -> Result<(), HarnessError> {
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        self.decisions
            .lock()
            .expect("fake mutex")
            .push(decision.clone());
        self.resolution_entered.add_permits(1);
        if self.hold_resolution.load(Ordering::SeqCst) {
            self.resolution_gate
                .acquire()
                .await
                .expect("resolution gate")
                .forget();
        }
        if self.refuse_resolution.load(Ordering::SeqCst) {
            return Err(HarnessError::Unsupported(
                "Not answered by this agent".into(),
            ));
        }
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), HarnessError> {
        Ok(())
    }
}
#[derive(Debug)]
pub struct FakeDesktop {
    pub target: Mutex<ControlTarget>,
    pub input: Arc<FakeInput>,
    pub captures: AtomicU64,
    pub locks: AtomicU64,
    /// Times it was unlocked, by `FakeDesktop::PASSWORD` alone.
    pub unlocks: AtomicU64,
}
impl FakeDesktop {
    pub const PASSWORD: &str = "fake desktop password";
}
impl Default for FakeDesktop {
    fn default() -> Self {
        Self {
            target: Mutex::new(ControlTarget {
                target_id: TargetId::new(),
                kind: TargetKind::Output,
                label: "Synthetic target".into(),
                native_ref: "test".into(),
                geometry: TargetGeometry {
                    width: 32,
                    height: 24,
                    scale: 1.0,
                    rotation_degrees: 0,
                    revision: 1,
                },
                capture: Availability::Available,
                input: Availability::Available,
            }),
            input: Arc::new(FakeInput::default()),
            captures: AtomicU64::new(0),
            locks: AtomicU64::new(0),
            unlocks: AtomicU64::new(0),
        }
    }
}
#[derive(Debug)]
pub struct FakeInput {
    pub injected: AtomicU64,
    pub releases: AtomicU64,
    pub hold_release: AtomicBool,
    pub release_gate: Semaphore,
    pub fail_release: AtomicBool,
}
impl Default for FakeInput {
    fn default() -> Self {
        Self {
            injected: AtomicU64::new(0),
            releases: AtomicU64::new(0),
            hold_release: AtomicBool::new(false),
            release_gate: Semaphore::new(0),
            fail_release: AtomicBool::new(false),
        }
    }
}
struct FakeSink(Arc<FakeInput>);
#[async_trait]
impl InputSink for FakeSink {
    async fn inject(&mut self, _: &InputAction, _: &TargetGeometry) -> Result<(), PlatformError> {
        self.0.injected.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn release_all(&mut self) -> Result<(), PlatformError> {
        // inject records synchronously; there are no queued synthetic actions.
        self.0.releases.fetch_add(1, Ordering::SeqCst);
        if self.0.hold_release.load(Ordering::SeqCst) {
            self.0
                .release_gate
                .acquire()
                .await
                .map_err(|_| PlatformError::Native("Synthetic closed gate".into()))?
                .forget();
        }
        if self.0.fail_release.load(Ordering::SeqCst) {
            Err(PlatformError::Native("Synthetic release failure".into()))
        } else {
            Ok(())
        }
    }
}
struct OneFrame(Option<CapturedFrame>);
#[async_trait]
impl FrameStream for OneFrame {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>, PlatformError> {
        Ok(self.0.take())
    }
}
#[async_trait]
impl DesktopBackend for FakeDesktop {
    async fn capabilities(&self) -> DesktopCapabilities {
        DesktopCapabilities {
            backend: "synthetic".into(),
            capture: Availability::Available,
            input: Availability::Available,
            input_scope: InputScope::OutputBound,
            lock_session: Availability::Available,
            unlock_session: Availability::Available,
        }
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
        Ok(vec![self.target.lock().expect("fake mutex").clone()])
    }
    async fn capture_still(&self, id: TargetId) -> Result<CapturedFrame, PlatformError> {
        let target = self.target.lock().expect("fake mutex");
        if target.target_id != id {
            return Err(PlatformError::TargetNotFound);
        }
        self.captures.fetch_add(1, Ordering::SeqCst);
        Ok(CapturedFrame {
            width: target.geometry.width,
            height: target.geometry.height,
            stride: target.geometry.width * 4,
            format: PixelFormat::Rgba8,
            data: vec![0; (target.geometry.width * target.geometry.height * 4) as usize],
            geometry: target.geometry,
            cursor: None,
            damage: None,
        })
    }
    async fn open_stream(
        &self,
        id: TargetId,
        _: u8,
    ) -> Result<Box<dyn FrameStream>, PlatformError> {
        Ok(Box::new(OneFrame(Some(self.capture_still(id).await?))))
    }
    async fn input_sink(&self, id: TargetId) -> Result<Box<dyn InputSink>, PlatformError> {
        if self.target.lock().expect("fake mutex").target_id != id {
            return Err(PlatformError::TargetNotFound);
        }
        Ok(Box::new(FakeSink(self.input.clone())))
    }
    async fn lock_session(&self) -> Result<(), PlatformError> {
        self.locks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn unlock_session(
        &self,
        password: &protocol::accounts::Secret,
        allowed: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<ports::UnlockOutcome, PlatformError> {
        if password.expose() != Self::PASSWORD {
            return Ok(ports::UnlockOutcome::Wrong);
        }
        if !allowed() {
            return Ok(ports::UnlockOutcome::Withheld);
        }
        self.unlocks.fetch_add(1, Ordering::SeqCst);
        Ok(ports::UnlockOutcome::Released)
    }
    /// Locked once asked to lock, until the password unlocks it.
    async fn desktop_lock(&self) -> protocol::control::DesktopLockState {
        match (
            self.locks.load(Ordering::SeqCst),
            self.unlocks.load(Ordering::SeqCst),
        ) {
            (0, 0) => protocol::control::DesktopLockState::Unknown,
            (locks, unlocks) if unlocks >= locks => protocol::control::DesktopLockState::Unlocked,
            _ => protocol::control::DesktopLockState::Locked,
        }
    }
}
#[derive(Debug, Default)]
pub struct FakeMedia;
struct FakeStreamControl;
#[async_trait]
impl EncodedStreamControl for FakeStreamControl {
    async fn ack(&self, _: u64) {}
    async fn adjust(&self, _: protocol::media::StreamParams) {}
    async fn request_keyframe(&self) {}
    async fn close(&self) {}
}
impl MediaEncoder for FakeMedia {
    fn start(
        &self,
        mut stream: Box<dyn FrameStream>,
        _: protocol::media::StreamParams,
    ) -> EncodedStream {
        let (tx, output) = mpsc::channel(8);
        tokio::spawn(async move {
            if let Ok(Some(f)) = stream.next_frame().await {
                let header = protocol::media::TileFrameHeader {
                    keyframe: true,
                    cursor: None,
                    tile: 128,
                    seq: 1,
                    geometry_revision: f.geometry.revision,
                    enc_width: f.width as u16,
                    enc_height: f.height as u16,
                    src_width: f.width,
                    src_height: f.height,
                    region: None,
                    count: 0,
                };
                let bytes = protocol::media::build_tile_frame(header, &[]);
                let _ = tx.send(EncodedOut::Frame(bytes.into())).await;
            }
        });
        EncodedStream {
            output,
            control: Arc::new(FakeStreamControl),
        }
    }
    fn encode_still(
        &self,
        frame: &CapturedFrame,
        _: StillFormat,
        _: Option<u32>,
    ) -> Result<EncodedStill, PlatformError> {
        Ok(EncodedStill {
            bytes: bytes::Bytes::from_static(b"synthetic image bytes"),
            media_type: "image/png",
            width: frame.width,
            height: frame.height,
        })
    }
}
#[derive(Debug, Default)]
pub struct FakeTerminal {
    sessions: Mutex<HashMap<TerminalId, FakeTerminalSession>>,
}
#[derive(Debug)]
struct FakeTerminalSession {
    info: protocol::terminal::TerminalInfo,
    running_children: Option<bool>,
    bytes: Vec<u8>,
    /// What was typed into it.
    input: Vec<u8>,
    tx: broadcast::Sender<TerminalEvent>,
}
impl FakeTerminal {
    pub fn set_running_children(&self, id: TerminalId, running: Option<bool>) {
        self.sessions
            .lock()
            .expect("fake mutex")
            .get_mut(&id)
            .expect("terminal")
            .running_children = running;
    }
    /// What has been typed into the terminal.
    pub fn input(&self, id: TerminalId) -> Vec<u8> {
        let sessions = self.sessions.lock().expect("fake mutex");
        sessions
            .get(&id)
            .map(|s| s.input.clone())
            .unwrap_or_default()
    }
    pub fn output(&self, id: TerminalId, bytes: &[u8]) {
        let mut sessions = self.sessions.lock().expect("fake mutex");
        let s = sessions.get_mut(&id).expect("terminal");
        let offset = s.bytes.len() as u64;
        s.bytes.extend_from_slice(bytes);
        s.info.output_offset = s.bytes.len() as u64;
        let _ = s.tx.send(TerminalEvent::Output {
            offset,
            bytes: bytes.to_vec().into(),
        });
    }
}
#[async_trait]
impl TerminalHost for FakeTerminal {
    async fn open(
        &self,
        spec: TerminalSpec,
    ) -> Result<protocol::terminal::TerminalInfo, TerminalError> {
        let id = TerminalId::new();
        let info = protocol::terminal::TerminalInfo {
            terminal_id: id,
            title: spec.title,
            cwd: spec.cwd.to_string_lossy().into_owned(),
            project_id: spec.project_id,
            harness: spec.harness,
            cols: spec.cols,
            rows: spec.rows,
            state: protocol::terminal::TerminalState::Running,
            created_at: Timestamp(0),
            output_offset: 0,
            last_output_at: None,
        };
        let (tx, _) = broadcast::channel(64);
        self.sessions.lock().expect("fake mutex").insert(
            id,
            FakeTerminalSession {
                info: info.clone(),
                running_children: None,
                bytes: Vec::new(),
                input: Vec::new(),
                tx,
            },
        );
        Ok(info)
    }
    async fn list(&self) -> Vec<protocol::terminal::TerminalInfo> {
        self.sessions
            .lock()
            .expect("fake mutex")
            .values()
            .map(|s| s.info.clone())
            .collect()
    }
    async fn info(&self, id: TerminalId) -> Option<protocol::terminal::TerminalInfo> {
        self.sessions
            .lock()
            .expect("fake mutex")
            .get(&id)
            .map(|s| s.info.clone())
    }
    async fn has_running_children(&self, id: TerminalId) -> Result<Option<bool>, TerminalError> {
        self.sessions
            .lock()
            .expect("fake mutex")
            .get(&id)
            .map(|s| s.running_children)
            .ok_or(TerminalError::NotFound)
    }
    async fn attach(
        &self,
        id: TerminalId,
        from: Option<u64>,
    ) -> Result<(Replay, broadcast::Receiver<TerminalEvent>), TerminalError> {
        let sessions = self.sessions.lock().expect("fake mutex");
        let s = sessions.get(&id).ok_or(TerminalError::NotFound)?;
        let from = from.unwrap_or(0).min(s.bytes.len() as u64);
        Ok((
            Replay {
                from,
                truncated: false,
                bytes: s.bytes[from as usize..].to_vec().into(),
            },
            s.tx.subscribe(),
        ))
    }
    async fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), TerminalError> {
        let mut sessions = self.sessions.lock().expect("fake mutex");
        let s = sessions.get_mut(&id).ok_or(TerminalError::NotFound)?;
        s.input.extend_from_slice(bytes);
        Ok(())
    }
    async fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), TerminalError> {
        let mut sessions = self.sessions.lock().expect("fake mutex");
        let s = sessions.get_mut(&id).ok_or(TerminalError::NotFound)?;
        s.info.cols = cols;
        s.info.rows = rows;
        let _ = s.tx.send(TerminalEvent::Resized { cols, rows });
        Ok(())
    }
    async fn close(&self, id: TerminalId) -> Result<(), TerminalError> {
        let mut sessions = self.sessions.lock().expect("fake mutex");
        let s = sessions.get_mut(&id).ok_or(TerminalError::NotFound)?;
        s.info.state = protocol::terminal::TerminalState::Exited { code: Some(0) };
        let _ = s.tx.send(TerminalEvent::Exited { code: Some(0) });
        Ok(())
    }
}
#[derive(Debug, Default)]
pub struct FakeSpeech;
struct FakeSpeechSession(u32);
#[async_trait]
impl SpeechEngine for FakeSpeech {
    fn name(&self) -> String {
        "Synthetic speech".into()
    }
    async fn availability(&self) -> Availability {
        Availability::Available
    }
    async fn start(&self, _: Option<String>) -> Result<Box<dyn SpeechSession>, SpeechError> {
        Ok(Box::new(FakeSpeechSession(0)))
    }
}
#[async_trait]
impl SpeechSession for FakeSpeechSession {
    async fn push_pcm(
        &mut self,
        samples: &[i16],
    ) -> Result<Vec<protocol::annotation::SpeechSegment>, SpeechError> {
        self.0 += samples.len() as u32;
        Ok(Vec::new())
    }
    async fn finish(&mut self) -> Result<Vec<protocol::annotation::SpeechSegment>, SpeechError> {
        Ok(vec![protocol::annotation::SpeechSegment {
            segment_id: "test".into(),
            text: "Synthetic transcript".into(),
            start_ms: 0,
            end_ms: self.0 / 16,
            revision: 1,
            is_final: true,
        }])
    }
}

/// Exact-byte revision model for Library service tests; never reads host files.
#[derive(Debug)]
pub struct FakeLibrary {
    roots: Mutex<Vec<RootSpec>>,
    files: Mutex<HashMap<(LibraryRootId, String), String>>,
    instructions: Mutex<Vec<protocol::library::InstructionSource>>,
    purges: std::sync::atomic::AtomicUsize,
    changes: broadcast::Sender<LibraryChange>,
    pub download_file: Mutex<Option<std::fs::File>>,
    pub hold_read: AtomicBool,
    pub read_entered: Semaphore,
    pub read_gate: Semaphore,
}
impl Default for FakeLibrary {
    fn default() -> Self {
        let (changes, _) = broadcast::channel(64);
        Self {
            roots: Mutex::new(Vec::new()),
            files: Mutex::new(HashMap::new()),
            instructions: Mutex::new(Vec::new()),
            purges: std::sync::atomic::AtomicUsize::new(0),
            changes,
            download_file: Mutex::new(None),
            hold_read: AtomicBool::new(false),
            read_entered: Semaphore::new(0),
            read_gate: Semaphore::new(0),
        }
    }
}
fn revision(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl FakeLibrary {
    /// How often the companion has asked for deleted documents to be purged.
    pub fn purges(&self) -> usize {
        self.purges.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// What every later instruction inspection reports, whatever the project.
    pub fn set_instruction_sources(&self, sources: Vec<protocol::library::InstructionSource>) {
        *self.instructions.lock().expect("fake mutex") = sources;
    }
    fn allowed(&self, id: LibraryRootId, path: &str, write: bool) -> Result<(), LibraryError> {
        crate::transfers::relative(path, true)
            .map_err(|_| LibraryError::PathNotAllowed("synthetic".into()))?;
        let roots = self.roots.lock().expect("fake mutex");
        let root = roots
            .iter()
            .find(|r| r.root_id == id)
            .ok_or(LibraryError::UnknownRoot)?;
        if write && !root.writable {
            return Err(LibraryError::ReadOnly);
        }
        Ok(())
    }
    fn content(id: LibraryRootId, path: &str, text: String) -> protocol::library::FileContent {
        protocol::library::FileContent {
            root_id: id,
            path: path.into(),
            revision: revision(&text),
            size: text.len() as u64,
            text,
            encoding: protocol::library::TextEncoding::Utf8,
            newline: protocol::library::NewlineStyle::Lf,
            has_bom: false,
            modified_at: Some(Timestamp(0)),
        }
    }
}
#[async_trait]
impl LibraryFs for FakeLibrary {
    async fn set_roots(&self, roots: Vec<RootSpec>) -> Result<(), LibraryError> {
        *self.roots.lock().expect("fake mutex") = roots;
        Ok(())
    }
    async fn roots(&self) -> Vec<protocol::library::LibraryRoot> {
        self.roots
            .lock()
            .expect("fake mutex")
            .iter()
            .map(|r| protocol::library::LibraryRoot {
                root_id: r.root_id,
                label: r.label.clone(),
                host_path: r.path.to_string_lossy().into_owned(),
                kind: r.kind,
                writable: r.writable,
                project_id: r.project_id,
                harness: r.harness,
                only: r.only.clone(),
            })
            .collect()
    }
    async fn list(
        &self,
        id: LibraryRootId,
        dir: &str,
    ) -> Result<protocol::library::DirListing, LibraryError> {
        self.allowed(id, dir, false)?;
        let entries = self
            .files
            .lock()
            .expect("fake mutex")
            .iter()
            .filter(|((root, path), _)| *root == id && path.starts_with(dir))
            .map(|((_, path), text)| protocol::library::DirEntry {
                hidden: path.rsplit('/').next().is_some_and(|n| n.starts_with('.')),
                name: path.clone(),
                path: path.clone(),
                kind: protocol::library::EntryKind::Markdown,
                size: text.len() as u64,
                modified_at: None,
                title: None,
                description: None,
            })
            .collect();
        Ok(protocol::library::DirListing {
            root_id: id,
            dir: dir.into(),
            entries,
        })
    }
    async fn read(
        &self,
        id: LibraryRootId,
        path: &str,
    ) -> Result<protocol::library::FileContent, LibraryError> {
        self.allowed(id, path, false)?;
        if self.hold_read.load(Ordering::SeqCst) {
            self.read_entered.add_permits(1);
            let _ = self.read_gate.acquire().await;
        }
        let text = self
            .files
            .lock()
            .expect("fake mutex")
            .get(&(id, path.into()))
            .cloned()
            .ok_or(LibraryError::NotFound)?;
        Ok(Self::content(id, path, text))
    }
    async fn open(&self, id: LibraryRootId, path: &str) -> Result<std::fs::File, LibraryError> {
        // The fixture owns the bytes; use the same gate as text reads.
        self.read(id, path).await?;
        self.download_file
            .lock()
            .expect("fake mutex")
            .as_ref()
            .ok_or(LibraryError::NotFound)?
            .try_clone()
            .map_err(LibraryError::Io)
    }
    async fn save(
        &self,
        id: LibraryRootId,
        path: &str,
        request: SaveRequest,
    ) -> Result<protocol::library::SaveOutcome, LibraryError> {
        self.allowed(id, path, true)?;
        let mut files = self.files.lock().expect("fake mutex");
        let Some(text) = files.get_mut(&(id, path.into())) else {
            return Ok(protocol::library::SaveOutcome::Missing);
        };
        if revision(text) != request.base_revision {
            return Ok(protocol::library::SaveOutcome::Conflict {
                current: Some(Box::new(Self::content(id, path, text.clone()))),
            });
        }
        *text = request.text;
        let revision = revision(text);
        let _ = self.changes.send(LibraryChange {
            root_id: id,
            path: path.into(),
            revision: Some(revision.clone()),
        });
        Ok(protocol::library::SaveOutcome::Saved { revision })
    }
    async fn create(
        &self,
        id: LibraryRootId,
        path: &str,
        text: &str,
    ) -> Result<String, LibraryError> {
        self.allowed(id, path, true)?;
        let mut files = self.files.lock().expect("fake mutex");
        if files.contains_key(&(id, path.into())) {
            return Err(LibraryError::AlreadyExists);
        }
        files.insert((id, path.into()), text.into());
        let revision = revision(text);
        let _ = self.changes.send(LibraryChange {
            root_id: id,
            path: path.into(),
            revision: Some(revision.clone()),
        });
        Ok(revision)
    }
    async fn rename(
        &self,
        id: LibraryRootId,
        from: &str,
        to: &str,
        base: &str,
    ) -> Result<String, LibraryError> {
        self.allowed(id, from, true)?;
        self.allowed(id, to, true)?;
        let mut files = self.files.lock().expect("fake mutex");
        if files.contains_key(&(id, to.into())) {
            return Err(LibraryError::AlreadyExists);
        }
        let text = files
            .get(&(id, from.into()))
            .ok_or(LibraryError::NotFound)?;
        if revision(text) != base {
            return Err(LibraryError::Conflict);
        }
        let text = files
            .remove(&(id, from.into()))
            .ok_or(LibraryError::NotFound)?;
        let revision = revision(&text);
        files.insert((id, to.into()), text);
        Ok(revision)
    }
    async fn trash(
        &self,
        id: LibraryRootId,
        path: &str,
        base: &str,
    ) -> Result<TrashEntry, LibraryError> {
        self.allowed(id, path, true)?;
        let mut files = self.files.lock().expect("fake mutex");
        let text = files
            .get(&(id, path.into()))
            .ok_or(LibraryError::NotFound)?;
        if revision(text) != base {
            return Err(LibraryError::Conflict);
        }
        let text = files
            .remove(&(id, path.into()))
            .ok_or(LibraryError::NotFound)?;
        let revision = revision(&text);
        files.insert((id, format!(".trash/{path}")), text);
        Ok(TrashEntry {
            trash_id: path.into(),
            root_id: id,
            original_path: path.into(),
            revision,
            stored_at: format!(".trash/{path}").into(),
        })
    }
    async fn restore(&self, trash_id: &str) -> Result<LibraryChange, LibraryError> {
        let mut files = self.files.lock().expect("fake mutex");
        let key = files
            .keys()
            .find(|(_, path)| path == &format!(".trash/{trash_id}"))
            .cloned()
            .ok_or(LibraryError::NotFound)?;
        if files.contains_key(&(key.0, trash_id.into())) {
            return Err(LibraryError::AlreadyExists);
        }
        let text = files.remove(&key).ok_or(LibraryError::NotFound)?;
        let revision = revision(&text);
        files.insert((key.0, trash_id.into()), text);
        Ok(LibraryChange {
            root_id: key.0,
            path: trash_id.into(),
            revision: Some(revision),
        })
    }
    async fn search(
        &self,
        id: Option<LibraryRootId>,
        text: &str,
        limit: u32,
    ) -> Result<Vec<protocol::library::SearchHit>, LibraryError> {
        Ok(self
            .files
            .lock()
            .expect("fake mutex")
            .iter()
            .filter(|((root, _), body)| id.is_none_or(|id| id == *root) && body.contains(text))
            .take(limit as usize)
            .map(|((root, path), body)| protocol::library::SearchHit {
                root_id: *root,
                path: path.clone(),
                line: Some(1),
                snippet: body.clone(),
            })
            .collect())
    }
    async fn instruction_sources(
        &self,
        _: &std::path::Path,
        _: HarnessKind,
        _: &[std::path::PathBuf],
        _: bool,
    ) -> Vec<protocol::library::InstructionSource> {
        self.instructions.lock().expect("fake mutex").clone()
    }
    async fn purge(&self) -> Result<(), LibraryError> {
        self.purges
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    fn changes(&self) -> broadcast::Receiver<LibraryChange> {
        self.changes.subscribe()
    }
}

/// Pairs a device the way the local owner does: the request waits until the
/// owner confirms the code it shows.
pub async fn pair_confirmed(
    hub: &dyn crate::Hub,
    request: PairRequest,
) -> Result<auth::PairResponse, ProtocolError> {
    let code = auth::pairing_code("", &request.pairing_secret, &request.device_public_key);
    let claim = hub.request_pairing(request, code.clone()).await?;
    hub.answer_pairing(&crate::Caller::local_owner(), &code, true)
        .await?;
    claim
        .answer
        .await
        .map_err(|_| ProtocolError::new(ErrorCode::Internal, "pairing answer lost"))?
}
