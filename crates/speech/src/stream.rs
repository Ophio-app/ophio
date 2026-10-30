// SPDX-License-Identifier: Apache-2.0
//! Capture-clock windowing. Times describe voiced audio spans, not forced word
//! alignment. A finalized span is never reopened, including at window rollover.
use crate::decoder::{DecodeError, Decoder, check_cancel};
use protocol::annotation::SpeechSegment;
use std::sync::atomic::AtomicBool;

pub const SAMPLE_RATE: usize = 16_000;
pub const MAX_SESSION_SAMPLES: usize = SAMPLE_RATE * 600;
pub const MAX_PUSH_SAMPLES: usize = SAMPLE_RATE;
const FRAME: usize = 320;
const PRE_ROLL: usize = 1600;
const HOP: usize = SAMPLE_RATE;
const WINDOW: usize = SAMPLE_RATE * 8;
const OVERLAP: usize = SAMPLE_RATE / 2;
const SILENCE: usize = SAMPLE_RATE * 32 / 100;
const AGREEMENT_SILENCE: usize = SAMPLE_RATE * 24 / 100;

pub(crate) struct StreamState {
    prefix: String,
    next_id: u32,
    pub(crate) accepted: usize,
    processed: usize,
    remainder: Vec<i16>,
    preroll: Vec<i16>,
    active: Option<Active>,
    final_end: usize,
}
struct Active {
    audio: Vec<i16>,
    start: usize,
    end: usize,
    last_decode: usize,
    context: String,
    fresh: bool,
    segment: Option<SpeechSegment>,
}

impl StreamState {
    pub(crate) fn new(prefix: String) -> Self {
        Self {
            prefix,
            next_id: 0,
            accepted: 0,
            processed: 0,
            remainder: Vec::new(),
            preroll: Vec::new(),
            active: None,
            final_end: 0,
        }
    }

    pub(crate) fn push(
        &mut self,
        samples: &[i16],
        decoder: &mut dyn Decoder,
        cancel: &AtomicBool,
    ) -> Result<Vec<SpeechSegment>, DecodeError> {
        self.accepted += samples.len();
        let mut frames = std::mem::take(&mut self.remainder);
        frames.extend_from_slice(samples);
        let mut out = Vec::new();
        let (chunks, remainder) = frames.as_chunks::<FRAME>();
        for frame in chunks {
            self.frame(frame, decoder, cancel, &mut out)?;
        }
        self.remainder.extend_from_slice(remainder);
        Ok(out)
    }

    pub(crate) fn finish(
        &mut self,
        decoder: &mut dyn Decoder,
        cancel: &AtomicBool,
    ) -> Result<Vec<SpeechSegment>, DecodeError> {
        let mut out = Vec::new();
        let tail = std::mem::take(&mut self.remainder);
        if !tail.is_empty() {
            self.frame(&tail, decoder, cancel, &mut out)?;
        }
        if let Some(mut active) = self.active.take()
            && active.fresh
        {
            self.decode(&mut active, decoder, cancel, true, &mut out)?;
        }
        self.preroll.clear();
        Ok(out)
    }

    fn frame(
        &mut self,
        frame: &[i16],
        decoder: &mut dyn Decoder,
        cancel: &AtomicBool,
        out: &mut Vec<SpeechSegment>,
    ) -> Result<(), DecodeError> {
        check_cancel(cancel)?;
        let start = self.processed;
        self.processed += frame.len();
        let voiced = voiced(frame);
        if self.active.is_none() && voiced {
            self.active = Some(Active {
                audio: std::mem::take(&mut self.preroll),
                start: start.max(self.final_end),
                end: self.processed,
                last_decode: start,
                context: String::new(),
                fresh: true,
                segment: None,
            });
        }
        let Some(mut active) = self.active.take() else {
            self.preroll.extend_from_slice(frame);
            let excess = self.preroll.len().saturating_sub(PRE_ROLL);
            self.preroll.drain(..excess);
            return Ok(());
        };
        active.audio.extend_from_slice(frame);
        if voiced {
            active.end = self.processed;
            active.fresh = true;
        }
        let silence = self.processed.saturating_sub(active.end);
        let rollover = active.audio.len() >= WINDOW;
        let final_now = silence >= SILENCE || rollover;
        let agreement_probe = silence >= AGREEMENT_SILENCE && active.last_decode < active.end;
        if active.fresh
            && (self.processed - active.last_decode >= HOP || final_now || agreement_probe)
        {
            let previous = active.segment.as_ref().map(|s| s.text.clone());
            self.decode(&mut active, decoder, cancel, final_now, out)?;
            if !final_now
                && agreement_probe
                && previous.is_some()
                && previous.as_deref() == active.segment.as_ref().map(|s| s.text.as_str())
            {
                self.emit(&mut active, None, true, out);
            }
        }
        let finalized = final_now || active.segment.as_ref().is_some_and(|s| s.is_final);
        if finalized {
            self.final_end = self.final_end.max(active.end);
            if rollover && silence < AGREEMENT_SILENCE {
                let context = active.segment.take().map_or_else(String::new, |s| s.text);
                let keep = active.audio.len().saturating_sub(OVERLAP);
                active.audio.drain(..keep);
                self.active = Some(Active {
                    audio: active.audio,
                    start: self.final_end,
                    end: self.final_end,
                    last_decode: self.processed,
                    context,
                    fresh: false,
                    segment: None,
                });
            } else {
                let keep = active.audio.len().saturating_sub(PRE_ROLL);
                self.preroll = active.audio.split_off(keep);
            }
        } else {
            self.active = Some(active);
        }
        Ok(())
    }

    fn decode(
        &mut self,
        active: &mut Active,
        decoder: &mut dyn Decoder,
        cancel: &AtomicBool,
        final_now: bool,
        out: &mut Vec<SpeechSegment>,
    ) -> Result<(), DecodeError> {
        let text = decoder.decode(&active.audio, cancel)?;
        let text = trim_overlap(&active.context, &text);
        active.last_decode = self.processed;
        self.emit(active, Some(text), final_now, out);
        Ok(())
    }

    fn emit(
        &mut self,
        active: &mut Active,
        text: Option<String>,
        final_now: bool,
        out: &mut Vec<SpeechSegment>,
    ) {
        let text = text.unwrap_or_else(|| {
            active
                .segment
                .as_ref()
                .map_or_else(String::new, |s| s.text.clone())
        });
        if text.is_empty() && active.segment.is_none() {
            return;
        }
        let end_ms = (active.end / 16) as u32;
        if let Some(segment) = &mut active.segment {
            if segment.text == text && segment.end_ms == end_ms && segment.is_final == final_now {
                return;
            }
            debug_assert!(!segment.is_final);
            segment.text = text;
            segment.end_ms = segment.end_ms.max(end_ms);
            segment.revision += 1;
            segment.is_final = final_now;
        } else {
            self.next_id += 1;
            active.segment = Some(SpeechSegment {
                segment_id: format!("{}:{}", self.prefix, self.next_id),
                text,
                start_ms: (active.start / 16) as u32,
                end_ms,
                revision: 1,
                is_final: final_now,
            });
        }
        if let Some(segment) = &active.segment {
            out.push(segment.clone());
        }
    }
}

fn voiced(samples: &[i16]) -> bool {
    let count = samples.len() as f64;
    let sum: f64 = samples.iter().map(|&s| f64::from(s)).sum();
    let squares: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
    // Remove DC bias. This is energy gating, not a claim of speech detection.
    squares / count - (sum / count).powi(2) >= 200.0f64.powi(2)
}

fn trim_overlap(previous: &str, current: &str) -> String {
    let old: Vec<_> = previous.split_whitespace().collect();
    let new: Vec<_> = current.split_whitespace().collect();
    let norm = |word: &str| {
        word.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let overlap = (1..=old.len().min(new.len()).min(24))
        .rev()
        .find(|&n| {
            old[old.len() - n..]
                .iter()
                .zip(&new[..n])
                .all(|(a, b)| norm(a) == norm(b))
        })
        .unwrap_or(0);
    new[overlap..].join(" ")
}

#[cfg(test)]
mod tests;
