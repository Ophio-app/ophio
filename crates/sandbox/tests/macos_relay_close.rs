// SPDX-License-Identifier: Apache-2.0
//! A relay closed while another program is being started on the Mac. This is
//! a test program of its own because it holds a process start open for a
//! while, and with it every other start and every relay's private steps in
//! the same process: beside the relay's other tests, that would hold up their
//! relays and spend the deadlines some of them set.
#![cfg(target_os = "macos")]
use containment::inherit;
use ports::sandbox::{RelayBudget, RelayPolicy, RelayProvider, RelaySpec};
use sandbox::relay::Relay;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::unix::{net::UnixStream, process::CommandExt},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

/// A process being started holds a copy of every descriptor the companion
/// has, a relay's listener among them, until its program has loaded; on the
/// Mac it keeps for good one made during the start, as a socket is made and
/// only then marked close-on-exec. A relay closed during another run's start
/// must still be unreachable once its shutdown returns. Here the start is held
/// open until the test lets it go.
#[test]
#[allow(unsafe_code)]
fn a_relay_closed_while_a_program_starts_is_closed_when_shutdown_returns() {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, "fixture-provider-key\n").unwrap();
    let spec = RelaySpec {
        provider: RelayProvider::OpenAi,
        key_file,
        budget: RelayBudget {
            max_requests: 100,
            max_input_tokens: 1000,
            max_output_tokens: 1000,
            deadline: None,
        },
        policy: RelayPolicy::default(),
        log: dir.path().join("audit.jsonl"),
        on_signal: None,
        inspect: None,
        test_upstream: None,
    };
    let relay = Relay::start(dir.path(), spec).unwrap();
    let endpoint = SocketAddr::from(([127, 0, 0, 1], relay.port()));
    let (gate, release) = UnixStream::pair().unwrap();
    let (forked, has_forked) = UnixStream::pair().unwrap();
    let starter = thread::spawn(move || {
        let mut command = Command::new("/usr/bin/true");
        command.stdin(Stdio::null());
        // SAFETY: the child only writes and reads a byte on descriptors it
        // inherited, which allocates nothing and takes no lock.
        unsafe {
            command.pre_exec(move || {
                let _ = (&forked).write_all(&[1]);
                let _ = (&gate).read(&mut [0_u8]);
                Ok(())
            });
        }
        inherit::spawn(&mut command).unwrap().wait().unwrap()
    });
    (&has_forked).read_exact(&mut [0_u8]).unwrap();
    let closing = thread::spawn(move || {
        relay.shutdown();
        TcpStream::connect(endpoint).is_err()
    });
    thread::sleep(Duration::from_millis(300));
    (&release).write_all(&[1]).unwrap();
    assert!(starter.join().unwrap().success());
    assert!(
        closing.join().unwrap(),
        "the relay was still reachable after its shutdown returned"
    );
}
