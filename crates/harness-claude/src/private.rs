// SPDX-License-Identifier: Apache-2.0
//! Process evidence for the restricted computer specialist. Never examines argv,
//! environment, open files, or native credentials.
use ports::HarnessError;
use std::sync::atomic::{AtomicBool, Ordering};

/// Closed by Stop or a private pause. No prompt or held message is written
/// to the agent afterwards, and none is admitted.
#[derive(Default)]
pub(crate) struct Admission(AtomicBool);
impl Admission {
    pub(crate) fn close(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub(crate) fn check(&self) -> Result<(), HarnessError> {
        if self.0.load(Ordering::SeqCst) {
            Err(HarnessError::NotActive)
        } else {
            Ok(())
        }
    }
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn same_process(id: u32, born: u64, other_id: u32, other_born: u64) -> bool {
    id == other_id && born == other_born
}

#[cfg(windows)]
pub(crate) fn unverified(reason: impl std::fmt::Display) -> HarnessError {
    HarnessError::Protocol(format!("{reason}; privacy is unverified"))
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct Process {
    id: u32,
    parent: u32,
    group: u32,
    born: u64,
    live: bool,
}

#[cfg(not(windows))]
pub(crate) struct Tree {
    #[cfg(target_os = "linux")]
    group: u32,
    #[cfg(target_os = "linux")]
    members: Vec<Process>,
}
#[cfg(not(windows))]
impl Tree {
    pub(crate) fn capture(pid: u32) -> Result<Self, HarnessError> {
        #[cfg(target_os = "linux")]
        {
            let all = processes()?;
            let mut members: Vec<_> = all
                .iter()
                .filter(|p| p.id == pid || p.group == pid)
                .copied()
                .collect();
            loop {
                let added: Vec<_> = all
                    .iter()
                    .filter(|p| {
                        !members.iter().any(|m| m.id == p.id)
                            && members.iter().any(|m| m.id == p.parent)
                    })
                    .copied()
                    .collect();
                if added.is_empty() {
                    break;
                }
                members.extend(added);
            }
            Ok(Self {
                group: pid,
                members,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            Err(HarnessError::Unsupported(
                "Native process-tree pause is not verified on this platform".into(),
            ))
        }
    }
    pub(crate) fn confirm(&self) -> Result<(), HarnessError> {
        #[cfg(target_os = "linux")]
        {
            self.confirm_from(&processes()?)
        }
        #[cfg(not(target_os = "linux"))]
        Err(HarnessError::Unsupported(
            "Native process-tree pause is not verified on this platform".into(),
        ))
    }
    #[cfg(target_os = "linux")]
    fn confirm_from(&self, all: &[Process]) -> Result<(), HarnessError> {
        if all.iter().any(|p| {
            p.live
                && (p.group == self.group
                    || self
                        .members
                        .iter()
                        .any(|m| same_process(m.id, m.born, p.id, p.born)))
        }) {
            return Err(HarnessError::Protocol(
                "A native descendant is still running; privacy is unverified".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn processes() -> Result<Vec<Process>, HarnessError> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        // A status read while its process exits can come back incomplete, so a
        // bad one is read again, and excused only if the process is then gone.
        let process = match status(id, &dir)? {
            None => continue,
            Some(Ok(process)) => process,
            Some(Err(_)) => match status(id, &dir)? {
                None => continue,
                Some(Ok(process)) => process,
                Some(Err(_)) if !dir.exists() => continue,
                Some(Err(e)) => return Err(e),
            },
        };
        out.push(process);
    }
    Ok(out)
}

/// `None` once the process has exited.
#[cfg(target_os = "linux")]
fn status(
    id: u32,
    dir: &std::path::Path,
) -> Result<Option<Result<Process, HarnessError>>, HarnessError> {
    let value = match std::fs::read_to_string(dir.join("stat")) {
        Ok(value) => value,
        // A process may exit between reading the directory and its stat.
        // Linux can report either ENOENT or ESRCH for that race.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
            return Ok(None);
        }
        // Do not treat an unreadable census as evidence that nobody survived.
        Err(e) => return Err(e.into()),
    };
    Ok(Some(parse_status(id, &value)))
}

#[cfg(target_os = "linux")]
fn parse_status(id: u32, value: &str) -> Result<Process, HarnessError> {
    let fields: Vec<_> = value
        .rsplit_once(')')
        .ok_or_else(|| HarnessError::Protocol("Invalid process status".into()))?
        .1
        .split_whitespace()
        .collect();
    let number = |i: usize| -> Result<u64, HarnessError> {
        fields
            .get(i)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| HarnessError::Protocol("Incomplete process status".into()))
    };
    Ok(Process {
        id,
        parent: number(1)? as u32,
        group: number(2)? as u32,
        born: number(19)?,
        live: !matches!(fields.first(), Some(&"Z" | &"X")),
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn surviving_descendant_keeps_privacy_unverified_even_outside_original_group() {
        let me = Process {
            id: 41,
            parent: 1,
            group: 40,
            born: 100,
            live: true,
        };
        // A captured member remains covered after it moves out of the original
        // group. Birth time prevents confusing a later reused PID with this run.
        let evidence = Tree {
            group: u32::MAX,
            members: vec![me],
        };
        assert!(evidence.confirm_from(&[me]).is_err());
        let mut different_process = me;
        different_process.born = me.born.wrapping_add(1);
        assert!(
            Tree {
                group: u32::MAX,
                members: vec![different_process]
            }
            .confirm_from(&[me])
            .is_ok()
        );
    }

    #[test]
    fn status_reads_the_fields_after_the_last_parenthesis_and_rejects_a_short_one() {
        let line = "7 (a) b) S 3 5 5 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 900 0 0";
        let process = parse_status(7, line).unwrap();
        assert_eq!((process.parent, process.group, process.born), (3, 5, 900));
        assert!(process.live);
        assert!(parse_status(7, "7 (a) S 3 5").is_err());
        assert!(parse_status(7, "").is_err());
    }
}

#[cfg(test)]
mod confirmation_tests {
    use super::*;

    #[test]
    fn process_identity_rejects_reused_pid_and_different_pid() {
        assert!(same_process(42, 100, 42, 100));
        assert!(!same_process(42, 100, 42, 101));
        assert!(!same_process(42, 100, 43, 100));
    }

    #[test]
    fn closed_admission_requires_a_fresh_run() {
        let old = Admission::default();
        old.check().unwrap();
        old.close();
        old.close();
        assert!(matches!(old.check(), Err(HarnessError::NotActive)));
        Admission::default().check().unwrap();
        assert!(old.check().is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn private_pause_discards_queued_work_and_rejects_late_messages() {
        use crate::*;
        let (dir, adapter, mut spec) = crate::tests::fixture();
        spec.bridge.as_mut().unwrap().computer_use = true;
        spec.prompt.text = "wait".into();
        let mut run = adapter.start_run(spec.clone()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !matches!(run.events.recv().await, Some(HarnessEvent::TurnStarted)) {}
        })
        .await
        .unwrap();
        let followup = PromptInput {
            text: "held-private-followup".into(),
            images: vec![],
        };
        assert_eq!(
            run.control
                .send_message(
                    MessageRef(protocol::CommandId::new()),
                    followup.clone(),
                    MessageDelivery::NextTurn
                )
                .await
                .unwrap(),
            DeliveryState::QueuedForBoundary
        );
        run.control.pause_for_private().await.unwrap();
        assert!(matches!(
            run.control
                .send_message(
                    MessageRef(protocol::CommandId::new()),
                    followup,
                    MessageDelivery::Steer
                )
                .await,
            Err(HarnessError::NotActive)
        ));
        assert!(
            !std::fs::read_to_string(dir.path().join("stdin.jsonl"))
                .unwrap()
                .contains("held-private-followup")
        );
        spec.run_id = protocol::RunId::new();
        spec.prompt.text = "complete".into();
        let mut resumed = adapter.start_run(spec).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !matches!(
                resumed.events.recv().await,
                Some(HarnessEvent::Exited { .. })
            ) {}
        })
        .await
        .unwrap();
        assert!(
            run.control
                .send_message(
                    MessageRef(protocol::CommandId::new()),
                    PromptInput::default(),
                    MessageDelivery::Steer
                )
                .await
                .is_err()
        );
    }
}
