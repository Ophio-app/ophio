// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::collections::{HashMap, VecDeque};

struct Fake {
    texts: VecDeque<String>,
    calls: usize,
}
impl Fake {
    fn new(texts: &[&str]) -> Self {
        Self {
            texts: texts.iter().map(|s| s.to_string()).collect(),
            calls: 0,
        }
    }
}
impl Decoder for Fake {
    fn decode(&mut self, _: &[i16], _: &AtomicBool) -> Result<String, DecodeError> {
        self.calls += 1;
        Ok(self
            .texts
            .pop_front()
            .unwrap_or_else(|| "recognised speech".into()))
    }
}
fn tone(count: usize) -> Vec<i16> {
    (0..count)
        .map(|i| if i % 2 == 0 { 1200 } else { -1200 })
        .collect()
}
fn state() -> StreamState {
    StreamState::new("test".into())
}
fn push(s: &mut StreamState, decoder: &mut dyn Decoder, audio: &[i16]) -> Vec<SpeechSegment> {
    s.push(audio, decoder, &AtomicBool::new(false)).unwrap()
}

#[test]
fn partials_revise_then_final_is_immutable() {
    let mut s = state();
    let mut d = Fake::new(&["fix", "fix the button", "fix the button", "next"]);
    let a = push(&mut s, &mut d, &tone(16_000));
    let b = push(&mut s, &mut d, &tone(16_000));
    let c = push(&mut s, &mut d, &vec![0; 9600]);
    assert!(!a[0].is_final);
    assert_eq!(a[0].segment_id, b[0].segment_id);
    assert_eq!(b[0].revision, a[0].revision + 1);
    let final_segment = c.last().unwrap();
    assert!(final_segment.is_final);
    assert_eq!(final_segment.text, "fix the button");
    assert_eq!((final_segment.start_ms, final_segment.end_ms), (0, 2000));
    let next = push(&mut s, &mut d, &tone(16_000));
    assert!(
        next.iter()
            .all(|x| x.segment_id != final_segment.segment_id)
    );
    assert!(next[0].start_ms >= final_segment.end_ms);
}

#[test]
fn sample_clock_handles_silence_arbitrary_chunks_and_finish_tail() {
    let mut s = state();
    let mut d = Fake::new(&["hello"]);
    for chunk in vec![0; 12_347].chunks(37) {
        assert!(push(&mut s, &mut d, chunk).is_empty());
    }
    for chunk in tone(21_021).chunks(173) {
        push(&mut s, &mut d, chunk);
    }
    let last = s.finish(&mut d, &AtomicBool::new(false)).unwrap();
    let last = last.last().unwrap();
    assert!(last.is_final);
    assert_eq!(last.end_ms, (33_368 / 16) as u32);
    assert!(last.start_ms >= 760 && last.start_ms <= 780);
    assert_eq!(s.accepted, 33_368);
    assert_eq!(s.processed, 33_368);
}

#[test]
fn silence_and_dc_bias_do_not_invoke_decoder() {
    let mut s = state();
    let mut d = Fake::new(&[]);
    for samples in [vec![0; 16_000], vec![17; 16_000], vec![17; 16_000]] {
        assert!(push(&mut s, &mut d, &samples).is_empty());
    }
    assert!(
        s.finish(&mut d, &AtomicBool::new(false))
            .unwrap()
            .is_empty()
    );
    assert_eq!(d.calls, 0);
}

#[test]
fn rollover_is_bounded_monotonic_and_final_segments_never_change() {
    let mut s = state();
    let mut d = Fake::new(&[]);
    let mut seen: HashMap<String, SpeechSegment> = HashMap::new();
    let mut final_end = 0;
    for _ in 0..45 {
        let segments = push(&mut s, &mut d, &tone(16_000));
        assert!(s.active.as_ref().is_none_or(|a| a.audio.len() <= WINDOW));
        for seg in segments {
            assert!(seg.start_ms <= seg.end_ms && seg.end_ms <= (s.accepted / 16) as u32);
            if let Some(previous) = seen.get(&seg.segment_id) {
                assert!(!previous.is_final);
                assert!(seg.revision > previous.revision);
                assert_eq!(seg.start_ms, previous.start_ms);
                assert!(seg.end_ms >= previous.end_ms);
            }
            if seg.is_final {
                assert!(seg.start_ms >= final_end);
                final_end = seg.end_ms;
            }
            seen.insert(seg.segment_id.clone(), seg);
        }
    }
    assert!(final_end >= 8000);
}

#[test]
fn final_decode_can_retract_a_draft_and_context_dedup_is_exact() {
    let mut s = state();
    let mut d = Fake::new(&["hallucinated", ""]);
    let initial = push(&mut s, &mut d, &tone(16_000));
    let final_result = s.finish(&mut d, &AtomicBool::new(false)).unwrap();
    assert_eq!(initial[0].segment_id, final_result[0].segment_id);
    assert!(final_result[0].text.is_empty() && final_result[0].is_final);
    assert_eq!(
        trim_overlap("Fix the button.", "The button is red"),
        "is red"
    );
    assert_eq!(trim_overlap("blue", "blueberry"), "blueberry");
}

#[test]
fn changing_hypotheses_across_overlapping_windows_keep_absolute_time() {
    struct Different(usize);
    impl Decoder for Different {
        fn decode(&mut self, _: &[i16], _: &AtomicBool) -> Result<String, DecodeError> {
            self.0 += 1;
            Ok(format!("phrase {}", self.0))
        }
    }
    let mut s = state();
    let mut d = Different(0);
    let mut finals = Vec::new();
    for _ in 0..24 {
        finals.extend(
            push(&mut s, &mut d, &tone(16_000))
                .into_iter()
                .filter(|s| s.is_final),
        );
    }
    finals.extend(s.finish(&mut d, &AtomicBool::new(false)).unwrap());
    assert_eq!(finals[0].start_ms, 0);
    assert_eq!(finals[0].end_ms, 8000);
    assert_eq!(finals[1].start_ms, 8000);
    assert_eq!(finals[1].end_ms, 15_500);
    assert_eq!(finals.last().unwrap().end_ms, 24_000);
    for pair in finals.windows(2) {
        assert_ne!(pair[0].segment_id, pair[1].segment_id);
        assert!(pair[0].end_ms <= pair[1].start_ms);
    }
}
