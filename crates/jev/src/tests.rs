// SPDX-License-Identifier: Apache-2.0
use super::{questions::*, *};
use serde_json::json;
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
#[derive(Debug)]
struct Keys;
#[async_trait]
impl KeySource for Keys {
    async fn load(&self) -> Result<Option<Key>, Error> {
        Ok(Some(Key::new("synthetic-key".into())?))
    }
}
fn request() -> VoiceRequest {
    VoiceRequest {
        transcript: "click Open".into(),
        controls: vec![ControlLabel {
            id: "e1".into(),
            role: "button".into(),
            name: "Open".into(),
        }],
    }
}
fn good() -> serde_json::Value {
    let intent: BTreeMap<_, _> = ACTIONS
        .into_iter()
        .map(|k| (k, if k == "click" { 1. } else { 0. }))
        .collect();
    let score =
        |n: f64| json!({"type":"score","score":n,"confidence":1,"probabilities":{"0":1.-n,"1":n}});
    json!({"model":MODEL,"answers":{"intent":{"type":"choice","choice":"click","confidence":1,"probabilities":intent},"target":{"type":"choice","choice":"e1","confidence":1,"probabilities":{"none":0.,"e1":1.}},"complete":score(1.),"is_command":score(1.),"destructive":score(0.),"is_correction":score(0.)},"usage":{"input_tokens":110,"output_tokens":20}})
}
async fn endpoint(
    body: Vec<u8>,
    status: u16,
    stall: bool,
) -> (Jev, tokio::task::JoinHandle<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
    let job = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let end;
        loop {
            let mut buf = [0; 1024];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                end = at + 4;
                break;
            }
        }
        let header = std::str::from_utf8(&bytes[..end]).unwrap();
        let len: usize = header
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(|v| v.parse().unwrap())
            })
            .unwrap();
        while bytes.len() < end + len {
            let mut buf = [0; 1024];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
        }
        let request = serde_json::from_slice(&bytes[end..end + len]).unwrap();
        if stall {
            let mut buf = [0];
            let _ = socket.read(&mut buf).await;
        } else {
            let headers = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
        request
    });
    (
        Jev::build(Arc::new(Keys), url, Duration::from_millis(250), false).unwrap(),
        job,
    )
}
#[tokio::test]
async fn loopback_success_sends_only_the_transcript_and_control_labels() {
    let (jev, job) = endpoint(serde_json::to_vec(&good()).unwrap(), 200, false).await;
    assert_eq!(jev.connection().await, Connection::Connected);
    let answer = jev.assess_voice(&request()).await.unwrap();
    assert_eq!(answer.answers.target.choice, "e1");
    assert_eq!(answer.usage.input_tokens, Some(110));
    let wire = job.await.unwrap();
    assert_eq!(
        wire["state"],
        json!({"transcript":"click Open","controls":[{"id":"e1","role":"button","name":"Open"}]})
    );
    assert_eq!(wire["model"], MODEL);
    assert!(!format!("{jev:?}").contains("synthetic-key"));
    assert!(!format!("{:?}", request()).contains("Open"));
    assert_eq!(
        format!("{:?}", Key::new("synthetic-key".into()).unwrap()),
        "Key([redacted])"
    );
}
#[tokio::test]
async fn loopback_rejects_another_model_malformed_answers_refusal_and_times_out() {
    let mut model = good();
    model["model"] = json!("jev-latest");
    let mut outside = good();
    outside["answers"]["target"]["choice"] = json!("outside");
    for (body, status, stall, reason) in [
        (
            serde_json::to_vec(&model).unwrap(),
            200,
            false,
            Error::Malformed,
        ),
        (
            serde_json::to_vec(&outside).unwrap(),
            200,
            false,
            Error::Malformed,
        ),
        (b"not json".to_vec(), 200, false, Error::Malformed),
        (
            serde_json::to_vec(&good()).unwrap(),
            403,
            false,
            Error::Refused,
        ),
        (Vec::new(), 200, true, Error::Timeout),
    ] {
        let (jev, job) = endpoint(body, status, stall).await;
        assert_eq!(jev.assess_voice(&request()).await.unwrap_err(), reason);
        let _ = job.await.unwrap();
    }
}
