// SPDX-License-Identifier: Apache-2.0
//! Opt-in public-fixture recognition and CPU benchmark; no network requests.
use protocol::annotation::SpeechSegment;
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

#[tokio::test]
async fn installed_model_streaming_recognition() {
    let Some(dir) = std::env::var_os("SPEECH_TEST_MODEL_DIR") else {
        eprintln!("SKIPPED: set SPEECH_TEST_MODEL_DIR to run local model recognition");
        return;
    };
    let model = std::env::var("SPEECH_TEST_MODEL").unwrap_or_else(|_| speech::DEFAULT_MODEL.into());
    let wav = std::env::var_os("SPEECH_TEST_WAV")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/speech-models/jfk.wav")
        });
    let mut reader = hound::WavReader::open(wav).unwrap();
    assert_eq!(
        (
            reader.spec().sample_rate,
            reader.spec().channels,
            reader.spec().bits_per_sample
        ),
        (16_000, 1, 16)
    );
    let mut samples = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    samples.extend_from_slice(&[0; 16_000]);
    let load = Instant::now();
    let engine = speech::Installed::open_model(&PathBuf::from(dir), &model).unwrap();
    assert!(
        engine.availability().await.is_available(),
        "{:?}",
        engine.availability().await
    );
    let load_seconds = load.elapsed().as_secs_f64();
    let mut session = engine.start(Some("en".into())).await.unwrap();
    let start = Instant::now();
    let mut total_compute = Duration::ZERO;
    let mut first_partial = None;
    let mut max_lag = 0.0f64;
    let mut segments: HashMap<String, SpeechSegment> = HashMap::new();
    let mut seen_samples = 0;
    let mut partials = 0;
    for chunk in samples.chunks(3200) {
        seen_samples += chunk.len();
        let capture_end = Duration::from_secs_f64(seen_samples as f64 / 16_000.0);
        tokio::time::sleep_until((start + capture_end).into()).await;
        let inference = Instant::now();
        let revisions = session.push_pcm(chunk).await.unwrap();
        total_compute += inference.elapsed();
        for revision in revisions {
            assert!(revision.start_ms <= revision.end_ms);
            assert!(revision.end_ms <= (seen_samples / 16) as u32);
            if let Some(old) = segments.get(&revision.segment_id) {
                assert!(!old.is_final, "final segment changed");
                assert!(revision.revision > old.revision);
                assert_eq!(revision.start_ms, old.start_ms);
            }
            if !revision.is_final {
                partials += 1;
                first_partial.get_or_insert(start.elapsed().as_secs_f64());
            }
            max_lag = max_lag.max(start.elapsed().as_secs_f64() - capture_end.as_secs_f64());
            segments.insert(revision.segment_id.clone(), revision);
        }
    }
    let inference = Instant::now();
    for segment in session.finish().await.unwrap() {
        segments.insert(segment.segment_id.clone(), segment);
    }
    total_compute += inference.elapsed();
    assert!(segments.values().all(|s| s.is_final));
    let mut ordered: Vec<_> = segments.values().collect();
    ordered.sort_by_key(|s| s.start_ms);
    for pair in ordered.windows(2) {
        assert!(pair[0].end_ms <= pair[1].start_ms);
    }
    let transcript = ordered
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let expected =
        std::env::var("SPEECH_TEST_EXPECT").unwrap_or_else(|_| "fellow americans country".into());
    let normalized = transcript.to_lowercase();
    let hits = expected
        .split_whitespace()
        .filter(|word| normalized.contains(&word.to_lowercase()))
        .count();
    eprintln!(
        "MODEL={model} load_s={load_seconds:.3} audio_s={:.3} compute_s={:.3} RTF={:.3} first_partial_s={:.3} max_delivery_lag_s={max_lag:.3} partials={partials}\nTRANSCRIPT={transcript}",
        samples.len() as f64 / 16_000.0,
        total_compute.as_secs_f64(),
        total_compute.as_secs_f64() / (samples.len() as f64 / 16_000.0),
        first_partial.unwrap_or(f64::NAN)
    );
    assert!(partials > 0, "no streaming partials");
    assert!(
        hits * 3 >= expected.split_whitespace().count() * 2,
        "recognition did not contain enough expected words"
    );
}
