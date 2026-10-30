// SPDX-License-Identifier: Apache-2.0
use super::*;

struct FakeDecoder;
impl Decoder for FakeDecoder {
    fn decode(&mut self, _: &[i16], cancel: &AtomicBool) -> Result<String, decoder::DecodeError> {
        decoder::check_cancel(cancel)?;
        Ok("test words".into())
    }
}
fn engine(decoder: Box<dyn Decoder>) -> Installed {
    Installed {
        model_name: DEFAULT_MODEL,
        availability: Availability::Available,
        shared: Some(Arc::new(Shared {
            decoder: Mutex::new(decoder),
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
            sessions: Arc::new(Semaphore::new(MAX_SESSIONS)),
            inference: Arc::new(Semaphore::new(1)),
        })),
    }
}

#[tokio::test]
async fn missing_model_explains_install_and_start_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Installed::open(dir.path()).unwrap();
    let availability = engine.availability().await;
    assert!(!availability.is_available());
    assert!(format!("{availability:?}").contains("ophio voice install tiny.en"));
    assert!(matches!(
        engine.start(None).await,
        Err(SpeechError::Unavailable(_))
    ));
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[tokio::test]
async fn sessions_are_bounded_and_drop_releases_slot() {
    let e = engine(Box::new(FakeDecoder));
    let a = e.start(None).await.unwrap();
    let b = e.start(Some("en-US".into())).await.unwrap();
    assert!(matches!(e.start(None).await, Err(SpeechError::Unavailable(s)) if s.contains("busy")));
    drop(a);
    assert!(e.start(None).await.is_ok());
    drop(b);
    assert!(matches!(
        e.start(Some("fr".into())).await,
        Err(SpeechError::Unavailable(_))
    ));
}

#[tokio::test]
async fn bounds_are_explicit_and_rejected_input_does_not_advance_clock() {
    let e = engine(Box::new(FakeDecoder));
    let mut s = e.start(None).await.unwrap();
    assert!(s.push_pcm(&vec![0; MAX_PUSH_SAMPLES + 1]).await.is_err());
    for _ in 0..600 {
        assert!(s.push_pcm(&[0; SAMPLE_RATE]).await.unwrap().is_empty());
    }
    assert!(s.push_pcm(&[0]).await.is_err());
    assert!(s.finish().await.unwrap().is_empty());
    assert!(s.finish().await.unwrap().is_empty());
    assert!(s.push_pcm(&[0]).await.is_err());
}

#[tokio::test]
async fn dropping_an_in_flight_future_cancels_decoder_and_poisoned_session() {
    struct Blocking {
        entered: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl Decoder for Blocking {
        fn decode(
            &mut self,
            _: &[i16],
            cancel: &AtomicBool,
        ) -> Result<String, decoder::DecodeError> {
            self.entered.take().unwrap().send(()).unwrap();
            while !cancel.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            Err(decoder::DecodeError::Cancelled)
        }
    }
    let (entered, rx) = tokio::sync::oneshot::channel();
    let e = engine(Box::new(Blocking {
        entered: Some(entered),
    }));
    let mut session = e.start(None).await.unwrap();
    let samples: Vec<i16> = (0..16_000)
        .map(|n| if n % 2 == 0 { 1200 } else { -1200 })
        .collect();
    let mut push = session.push_pcm(&samples);
    tokio::select! { _ = &mut push => panic!("decoder should wait for cancellation"), _ = rx => {} }
    drop(push);
    assert!(session.push_pcm(&[0]).await.is_err());
    drop(session);
    let shared = e.shared.as_ref().unwrap();
    let _done = shared.inference.acquire().await.unwrap();
    assert_eq!(shared.sessions.available_permits(), MAX_SESSIONS);
}
