// SPDX-License-Identifier: Apache-2.0
use crate::tiles::{TileState, validate};
use async_trait::async_trait;
use ports::{
    desktop::{CapturedFrame, DamageRect, FrameStream, PlatformError},
    media::{EncodedOut, EncodedStream, EncodedStreamControl},
};
use protocol::{control::TargetGeometry, media::StreamParams};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc},
    time::Instant,
};

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}

/// How often a capture that changed nothing is still reported. Viewers treat a
/// longer silence as a stalled picture. Only a capture counts: a source that
/// delivers nothing, idle or stalled, is not vouched for.
const UNCHANGED_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct Settings {
    params: StreamParams,
    generation: u64,
    closed: bool,
    in_flight: BTreeSet<u64>,
}
#[derive(Debug)]
struct Control {
    settings: Mutex<Settings>,
    capture_abort: Mutex<Option<tokio::task::AbortHandle>>,
    wake: Arc<Notify>,
}
#[async_trait]
impl EncodedStreamControl for Control {
    async fn ack(&self, seq: u64) {
        lock(&self.settings).in_flight.remove(&seq);
        self.wake.notify_one();
    }
    async fn adjust(&self, params: StreamParams) {
        let mut settings = lock(&self.settings);
        settings.params = params;
        settings.generation = settings.generation.wrapping_add(1);
        self.wake.notify_one();
    }
    async fn request_keyframe(&self) {
        let mut settings = lock(&self.settings);
        settings.generation = settings.generation.wrapping_add(1);
        self.wake.notify_one();
    }
    async fn close(&self) {
        lock(&self.settings).closed = true;
        if let Some(capture) = lock(&self.capture_abort).as_ref() {
            capture.abort();
        }
        self.wake.notify_one();
    }
}

#[derive(Debug, Default)]
struct CaptureSlot {
    frame: Option<Arc<CapturedFrame>>,
    paused: bool,
    pause_pending: bool,
    ended: Option<&'static str>,
}

// A single bounding rectangle prevents damage metadata from growing with lag.
fn merge_damage(new: &mut CapturedFrame, old: &CapturedFrame) {
    if new.geometry != old.geometry || (new.width, new.height) != (old.width, old.height) {
        new.damage = None;
        return;
    }
    new.damage = match (&old.damage, &new.damage) {
        (Some(a), Some(b)) => {
            let mut bounds: Option<DamageRect> = None;
            for rect in a.iter().chain(b) {
                if rect.width == 0 || rect.height == 0 {
                    continue;
                }
                bounds = Some(match bounds {
                    None => *rect,
                    Some(previous) => {
                        let x = previous.x.min(rect.x);
                        let y = previous.y.min(rect.y);
                        DamageRect {
                            x,
                            y,
                            width: previous
                                .x
                                .saturating_add(previous.width)
                                .max(rect.x.saturating_add(rect.width))
                                .saturating_sub(x),
                            height: previous
                                .y
                                .saturating_add(previous.height)
                                .max(rect.y.saturating_add(rect.height))
                                .saturating_sub(y),
                        }
                    }
                });
            }
            Some(bounds.into_iter().collect())
        }
        _ => None,
    };
}

pub(crate) fn start(mut frames: Box<dyn FrameStream>, params: StreamParams) -> EncodedStream {
    let (output, receiver) = mpsc::channel(1);
    let wake = Arc::new(Notify::new());
    let control = Arc::new(Control {
        capture_abort: Mutex::new(None),
        settings: Mutex::new(Settings {
            params,
            generation: 1,
            closed: false,
            in_flight: BTreeSet::new(),
        }),
        wake: wake.clone(),
    });
    let slot = Arc::new(Mutex::new(CaptureSlot::default()));
    let capture_slot = slot.clone();
    let capture_wake = wake.clone();
    let capture = tokio::spawn(async move {
        loop {
            match frames.next_frame().await {
                Ok(Some(mut frame)) => {
                    let mut slot = lock(&capture_slot);
                    if let Some(old) = slot.frame.take() {
                        merge_damage(&mut frame, &old);
                    }
                    slot.frame = Some(Arc::new(frame));
                    slot.paused = false;
                }
                Ok(None) => {
                    lock(&capture_slot).ended = Some("capture ended");
                    capture_wake.notify_one();
                    break;
                }
                Err(
                    PlatformError::TemporarilyUnavailable(_) | PlatformError::PermissionMissing(_),
                ) => {
                    {
                        let mut slot = lock(&capture_slot);
                        if !slot.paused {
                            slot.pause_pending = true;
                        }
                        slot.paused = true;
                    }
                    capture_wake.notify_one();
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Err(_) => {
                    lock(&capture_slot).ended = Some("capture failed");
                    capture_wake.notify_one();
                    break;
                }
            }
            capture_wake.notify_one();
            // Prevent a misbehaving always-ready source from starving its consumer.
            tokio::task::yield_now().await;
        }
    });
    *lock(&control.capture_abort) = Some(capture.abort_handle());
    let worker_control = control.clone();
    tokio::spawn(async move {
        // Abort the source even when the transport drops its receiver or this worker fails.
        struct Abort(tokio::task::JoinHandle<()>);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _capture = Abort(capture);
        run(output, worker_control, slot).await;
    });
    EncodedStream {
        output: receiver,
        control,
    }
}

async fn run(
    output: mpsc::Sender<EncodedOut>,
    control: Arc<Control>,
    slot: Arc<Mutex<CaptureSlot>>,
) {
    let mut state = TileState::default();
    let mut last_frame: Option<Arc<CapturedFrame>> = None;
    let mut geometry: Option<TargetGeometry> = None;
    let mut source_size = None;
    let mut generation = 0;
    let mut seq = 0u64;
    let mut paused = false;
    let mut next_encode = Instant::now();
    let mut last_sent = Instant::now();
    loop {
        let notified = control.wake.notified();
        let (params, desired_generation, closed, in_flight) = {
            let s = lock(&control.settings);
            (s.params, s.generation, s.closed, s.in_flight.len())
        };
        if closed || output.is_closed() {
            break;
        }
        if validate(params).is_err() {
            end(&output, &control, "invalid stream parameters").await;
            break;
        }
        let (is_paused, ended, has_frame, pause_pending) = {
            let s = lock(&slot);
            (s.paused, s.ended, s.frame.is_some(), s.pause_pending)
        };
        if is_paused != paused || (pause_pending && !paused) {
            let announce_pause = is_paused || pause_pending;
            let event = if announce_pause {
                EncodedOut::Paused { reason: "capture is temporarily unavailable; check desktop permission or session lock".into() }
            } else {
                EncodedOut::Resumed
            };
            tokio::select! { _ = notified => continue, permit = output.reserve() => {
                let Ok(permit) = permit else { break }; permit.send(event); paused = announce_pause;
                if announce_pause {lock(&slot).pause_pending=false;}
                if !paused { generation = 0; }
            }}
            continue;
        }
        if let Some(reason) = ended.filter(|_| !has_frame && generation == desired_generation) {
            end(&output, &control, reason).await;
            break;
        }
        let quiet_until = last_sent + UNCHANGED_EVERY;
        let need_frame = has_frame || (generation != desired_generation && last_frame.is_some());
        if !paused
            && need_frame
            && in_flight < params.window as usize
            && Instant::now() >= next_encode
        {
            let permit = tokio::select! {
                _ = notified => continue,
                permit = output.reserve() => { let Ok(permit) = permit else { break }; permit }
            };
            // Read only after obtaining transport capacity; never encode a backlog.
            if let Some(frame) = lock(&slot).frame.take() {
                last_frame = Some(frame);
            }
            let Some(frame) = last_frame.clone() else {
                continue;
            };
            let changed = geometry != Some(frame.geometry)
                || source_size != Some((frame.width, frame.height));
            if changed && geometry.is_some() {
                permit.send(EncodedOut::GeometryChanged(frame.geometry));
                geometry = Some(frame.geometry);
                source_size = Some((frame.width, frame.height));
                generation = 0;
                continue;
            }
            geometry = Some(frame.geometry);
            source_size = Some((frame.width, frame.height));
            let keyframe = changed || generation != desired_generation;
            let Some(next_seq) = seq.checked_add(1) else {
                break;
            };
            let source = frame.clone();
            let job = tokio::task::spawn_blocking(move || {
                let result = state.encode(&source, params, next_seq, keyframe);
                (state, result)
            })
            .await;
            let Ok((returned, result)) = job else {
                permit.send(EncodedOut::Ended {
                    reason: "encoder worker failed".into(),
                });
                break;
            };
            state = returned;
            let settings_changed = {
                let s = lock(&control.settings);
                s.closed || s.generation != desired_generation
            };
            if settings_changed {
                state = TileState::default();
                generation = 0;
                continue;
            }
            generation = desired_generation;
            next_encode = Instant::now() + Duration::from_secs_f64(1.0 / f64::from(params.max_fps));
            // A capture that changed nothing still says so now and then.
            // Without it a viewer cannot tell a still picture from a stalled
            // one; a stalled source sends no capture and so says nothing.
            let result = match result {
                Ok(None) if Instant::now() >= quiet_until => Ok(state.unchanged(&frame, next_seq)),
                other => other,
            };
            match result {
                Ok(Some(frame)) => {
                    seq = next_seq;
                    lock(&control.settings).in_flight.insert(seq);
                    permit.send(EncodedOut::Frame(frame));
                    last_sent = Instant::now();
                }
                Ok(None) => {}
                Err(_) => {
                    permit.send(EncodedOut::Ended {
                        reason: "frame encoding failed".into(),
                    });
                    break;
                }
            }
            continue;
        }
        if ended.is_some() && !has_frame && last_frame.is_none() {
            end(&output, &control, ended.unwrap_or("capture ended")).await;
            break;
        }
        tokio::select! {
            _ = notified => {},
            _ = output.closed() => break,
            _ = tokio::time::sleep_until(next_encode), if need_frame && !paused && in_flight < params.window as usize && Instant::now() < next_encode => {},
        }
    }
}

async fn end(output: &mpsc::Sender<EncodedOut>, control: &Control, reason: &str) {
    loop {
        let notified = control.wake.notified();
        if lock(&control.settings).closed {
            return;
        }
        tokio::select! {
            _=notified=>{},
            permit=output.reserve()=>{if let Ok(permit)=permit{permit.send(EncodedOut::Ended{reason:reason.into()});}return;}
        }
    }
}
