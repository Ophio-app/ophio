// SPDX-License-Identifier: Apache-2.0
//! Opt-in, disposable native Job tests. No agent, account, desktop, host ACL,
//! service or profile is used. Every child is this authored test executable.
use super::*;
use crate::ResourceLimits;
use std::{path::Path, process::Stdio};
use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        JobObjects::IsProcessInJob,
        Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc},
        Threading::{CREATE_BREAKAWAY_FROM_JOB, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    },
};

const FIXTURE: &str = "job::limits_tests::fixture";
const MIB: usize = 1024 * 1024;

fn limits() -> ResourceLimits {
    ResourceLimits {
        memory_bytes: (128 * MIB) as u64,
        cpu_millis_per_second: 1000,
        processes: 8,
    }
}

fn fixture_command(path: &Path, role: &str) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", FIXTURE, "--ignored", "--nocapture"])
        .env_clear()
        .env("OPHIO_JOB_LIMIT_ROLE", role)
        .env("TEMP", path)
        .env("TMP", path)
        .current_dir(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
}

// Keep descendant handles alive while the fixture works, and clean them up if
// it unwinds. Normal termination is owned by the outer kill-on-close Job.
struct FixtureChild(std::process::Child);
impl Drop for FixtureChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_fixture(path: &Path, role: &str) -> FixtureChild {
    use std::os::windows::process::CommandExt;
    FixtureChild(
        fixture_command(path, role)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap(),
    )
}

fn command(path: &Path, role: &str) -> Command {
    fixture_command(path, role).into()
}

fn publish(name: &str, text: impl AsRef<[u8]>) {
    std::fs::write(format!("{name}.part"), text).unwrap();
    std::fs::rename(format!("{name}.part"), name).unwrap();
}

fn park() -> ! {
    loop {
        std::thread::park();
    }
}

fn burn(progress: &str) -> ! {
    let mut value = 1_u64;
    let mut iterations = 0_u64;
    loop {
        value = std::hint::black_box(value.wrapping_mul(1664525).wrapping_add(1013904223));
        iterations += 1;
        if iterations.is_multiple_of(65_536) {
            publish(progress, iterations.to_string());
        }
    }
}

fn allocate(megabytes: usize) -> usize {
    let mut committed = 0;
    for _ in 0..megabytes {
        // SAFETY: this asks Windows for fresh anonymous private memory in only
        // the disposable fixture. No address, file mapping or host handle is used.
        let page = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                MIB,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        if page.is_null() {
            break;
        }
        // Touch every page so this is not just a virtual-address reservation.
        for offset in (0..MIB).step_by(4096) {
            // SAFETY: the new committed allocation contains every touched byte.
            unsafe { page.cast::<u8>().add(offset).write_volatile(1) };
        }
        committed += 1;
    }
    committed
}

#[test]
#[ignore = "only invoked as an authored subprocess by the opt-in native Job tests"]
fn fixture() {
    let role = std::env::var("OPHIO_JOB_LIMIT_ROLE").unwrap();
    use std::os::windows::process::CommandExt;
    let path = std::env::current_dir().unwrap();
    match role.as_str() {
        "idle" => park(),
        "preallocated" => {
            assert_eq!(allocate(32), 32);
            publish("preallocated", b"ready");
            park();
        }
        "memory-child" => {
            let committed = allocate(512);
            publish("memory-exhausted", committed.to_string());
            park();
        }
        "memory" => {
            assert_eq!(allocate(32), 32);
            let child = spawn_fixture(&path, "memory-child");
            publish("child.pid", child.0.id().to_string());
            park();
        }
        "processes" => {
            let mut children = Vec::new();
            for _ in 0..16 {
                match fixture_command(&path, "idle")
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                {
                    Ok(child) => children.push(child),
                    Err(_) => {
                        publish("process-refused", children.len().to_string());
                        park();
                    }
                }
            }
            panic!("the native Job process limit did not refuse a child");
        }
        "cpu-child" => burn("cpu-child.progress"),
        "cpu" => {
            let child = spawn_fixture(&path, "cpu-child");
            publish("child.pid", child.0.id().to_string());
            publish("ready", b"ready");
            burn("cpu-parent.progress");
        }
        "nested" => {
            let nested = Job::new().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let child = runtime
                .block_on(nested.spawn(&mut command(&path, "idle")))
                .unwrap();
            publish("child.pid", child.id().unwrap().to_string());
            publish("ready", b"ready");
            // Keep both the nested Job and child handles alive until the outer
            // Job ends us. An inner Job cannot remove outer membership.
            std::hint::black_box((&nested, &child));
            park();
        }
        "breakaway" => {
            assert!(
                fixture_command(&path, "idle")
                    .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
                    .spawn()
                    .is_err()
            );
            publish("breakaway-refused", b"refused");
            park();
        }
        _ => panic!("unknown disposable Job fixture"),
    }
}

async fn wait_file(path: &Path) -> String {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("authored fixture did not publish its result")
}

fn extended(job: &Job) -> JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
    let mut result = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: live job; correctly sized initialized output is not retained.
    assert_ne!(
        unsafe {
            QueryInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&mut result as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&result) as u32,
                std::ptr::null_mut(),
            )
        },
        0
    );
    result
}

fn cpu_used(job: &Job) -> Duration {
    let mut result = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: live job and correctly sized writable native accounting output.
    assert_ne!(
        unsafe {
            QueryInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut result as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&result) as u32,
                std::ptr::null_mut(),
            )
        },
        0
    );
    Duration::from_nanos((result.TotalKernelTime + result.TotalUserTime) as u64 * 100)
}

fn process_cpu_used(process: HANDLE) -> Duration {
    let mut born = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: callers keep their authored fixture's handle alive with query
    // rights throughout this read; all four FILETIME outputs are writable.
    assert_ne!(
        unsafe { GetProcessTimes(process, &mut born, &mut exited, &mut kernel, &mut user) },
        0
    );
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}

async fn child_handle(path: &Path, job: &Job) -> OwnedHandle {
    let pid = wait_file(&path.join("child.pid"))
        .await
        .parse::<u32>()
        .unwrap();
    // SAFETY: this only opens synchronization/query rights to our inert fixture.
    let handle = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    assert!(!handle.is_null());
    // SAFETY: uniquely owned non-null OpenProcess result.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut member = 0;
    // SAFETY: owned live handles and a writable BOOL. Membership pins this
    // fixture's identity rather than trusting a potentially reused PID.
    assert_ne!(
        unsafe { IsProcessInJob(handle.as_raw_handle(), job.0.as_raw_handle(), &mut member) },
        0
    );
    assert_ne!(member, 0);
    handle
}

async fn end(job: &Job, child: &mut Child) {
    job.terminate().unwrap();
    job.confirm().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
#[ignore = "native enforcement unqualified: CPU used 5.23s vs 0.86s allowance; console hosts exceed process cap; disposable Windows only"]
async fn limited_container_rejects_preallocated_external_process_without_assigning_it() {
    let dir = tempfile::tempdir().unwrap();
    // The unlimited outer Job safely owns the fixture even if this test fails.
    let outer = Job::new().unwrap();
    let mut child = outer
        .spawn(&mut command(dir.path(), "preallocated"))
        .await
        .unwrap();
    wait_file(&dir.path().join("preallocated")).await;
    let mut requested = limits();
    requested.memory_bytes = (16 * MIB) as u64;
    // Exercise the retained native implementation directly. The public
    // constructor refuses all resource-limited Windows containers until the
    // CPU and process enforcement failures are resolved and qualified.
    let job = Job::limited(requested).unwrap();
    assert!(matches!(
        job.adopt(child.raw_handle().unwrap()),
        Err(HarnessError::Unsupported(_))
    ));
    assert_eq!(job.accounting().unwrap(), (0, 0));
    // The rejected process still lives in only its original Job. The limited
    // container did not pretend to charge its earlier 32 MiB commitment.
    assert!(child.try_wait().unwrap().is_none());
    let unlimited = crate::Container::new().unwrap();
    unlimited.adopt(child.raw_handle().unwrap()).unwrap();
    unlimited.end().await.unwrap();
    end(&outer, &mut child).await;
}

#[tokio::test]
#[ignore = "native enforcement unqualified: CPU used 5.23s vs 0.86s allowance; console hosts exceed process cap; disposable Windows only"]
async fn limits_are_installed_before_launch_and_process_cap_refuses_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let mut requested = limits();
    requested.processes = 4;
    let job = Job::limited(requested).unwrap();
    let policy = extended(&job);
    assert_eq!(policy.JobMemoryLimit, requested.memory_bytes as usize);
    assert_eq!(policy.BasicLimitInformation.ActiveProcessLimit, 4);
    assert_eq!(
        policy.BasicLimitInformation.LimitFlags,
        JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    );
    let mut cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
    // SAFETY: live job and correctly sized writable native policy output.
    assert_ne!(
        unsafe {
            QueryInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectCpuRateControlInformation,
                (&mut cpu as *mut JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                std::mem::size_of_val(&cpu) as u32,
                std::ptr::null_mut(),
            )
        },
        0
    );
    assert_eq!(
        cpu.ControlFlags,
        JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP
    );
    // SAFETY: HARD_CAP uses the CpuRate union member.
    assert!(unsafe { cpu.Anonymous.CpuRate } > 0);
    let mut child = job
        .spawn(&mut command(dir.path(), "processes"))
        .await
        .unwrap();
    let admitted = wait_file(&dir.path().join("process-refused"))
        .await
        .parse::<u32>()
        .unwrap();
    assert!(admitted > 0 && admitted < requested.processes);
    let accounting = job.accounting().unwrap();
    assert!(
        accounting.1 <= requested.processes,
        "requested {} processes; admitted {admitted} descendants; total={} active={}",
        requested.processes,
        accounting.0,
        accounting.1
    );
    end(&job, &mut child).await;
}

#[tokio::test]
#[ignore = "native enforcement unqualified: CPU used 5.23s vs 0.86s allowance; console hosts exceed process cap; disposable Windows only"]
async fn committed_memory_cap_is_aggregate_and_recovers_after_cleanup() {
    // The parent first commits 32 MiB; its child cannot commit the full Job
    // limit on top. Two fresh runs prove release/recovery, not just refusal.
    for _ in 0..2 {
        let dir = tempfile::tempdir().unwrap();
        let requested = limits();
        let job = Job::limited(requested).unwrap();
        let mut child = job.spawn(&mut command(dir.path(), "memory")).await.unwrap();
        let descendant = child_handle(dir.path(), &job).await;
        let committed = wait_file(&dir.path().join("memory-exhausted"))
            .await
            .parse::<usize>()
            .unwrap();
        assert!(committed > 0 && committed < 96);
        let policy = extended(&job);
        let peak = policy.PeakJobMemoryUsed;
        assert!(
            peak > 64 * MIB && peak <= requested.memory_bytes as usize,
            "Job peak={peak} bytes; process peak={} bytes; memory limit={} bytes; parent allocated 32 MiB, child allocated {committed} MiB; accounting={:?}",
            policy.PeakProcessMemoryUsed,
            policy.JobMemoryLimit,
            job.accounting().unwrap()
        );
        end(&job, &mut child).await;
        // SAFETY: synchronization-only wait on our still-owned descendant.
        assert_eq!(
            unsafe { WaitForSingleObject(descendant.as_raw_handle(), 3000) },
            WAIT_OBJECT_0
        );
    }
}

#[tokio::test]
#[ignore = "native enforcement unqualified: CPU used 5.23s vs 0.86s allowance; console hosts exceed process cap; disposable Windows only"]
async fn nested_job_cannot_escape_outer_limits_or_kill_on_close() {
    let dir = tempfile::tempdir().unwrap();
    let job = Job::limited(limits()).unwrap();
    let mut child = job.spawn(&mut command(dir.path(), "nested")).await.unwrap();
    wait_file(&dir.path().join("ready")).await;
    let descendant = child_handle(dir.path(), &job).await;
    // SAFETY: this only polls the fixture's synchronization handle.
    assert_eq!(
        unsafe { WaitForSingleObject(descendant.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    drop(job);
    // SAFETY: dropping the outer Job must end this nested descendant too.
    assert_eq!(
        unsafe { WaitForSingleObject(descendant.as_raw_handle(), 3000) },
        WAIT_OBJECT_0
    );
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    let job = Job::limited(limits()).unwrap();
    let mut child = job
        .spawn(&mut command(dir.path(), "breakaway"))
        .await
        .unwrap();
    wait_file(&dir.path().join("breakaway-refused")).await;
    end(&job, &mut child).await;
}

async fn measure_cpu(job: &Job) -> (Duration, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let mut child = job.spawn(&mut command(dir.path(), "cpu")).await.unwrap();
    wait_file(&dir.path().join("ready")).await;
    let descendant = child_handle(dir.path(), job).await;
    let mut policy = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
    // SAFETY: the Job stays live and the initialized output has the native size.
    assert_ne!(
        unsafe {
            QueryInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectCpuRateControlInformation,
                (&mut policy as *mut JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                std::mem::size_of_val(&policy) as u32,
                std::ptr::null_mut(),
            )
        },
        0
    );
    eprintln!(
        "CPU policy: processors={} flags={} rate={} accounting={:?}",
        // SAFETY: this is a read-only system processor census.
        unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) },
        policy.ControlFlags,
        // SAFETY: the anonymous union contains a DWORD in every policy mode;
        // this diagnostic reports its raw bits even for the disabled control.
        unsafe { policy.Anonymous.CpuRate },
        job.accounting().unwrap()
    );
    async fn progress(path: &Path) -> [u64; 2] {
        [
            wait_file(&path.join("cpu-parent.progress"))
                .await
                .parse()
                .unwrap(),
            wait_file(&path.join("cpu-child.progress"))
                .await
                .parse()
                .unwrap(),
        ]
    }
    fn alive(root: &mut Child, descendant: &OwnedHandle) {
        assert!(
            root.try_wait().unwrap().is_none(),
            "busy root exited before measurement completed"
        );
        // SAFETY: synchronization-only query to our owned, membership-checked
        // fixture. An exited worker cannot masquerade as successful throttling.
        assert_eq!(
            unsafe { WaitForSingleObject(descendant.as_raw_handle(), 0) },
            WAIT_TIMEOUT,
            "busy descendant exited before measurement completed"
        );
    }
    let mut previous = progress(dir.path()).await;
    let mut total_wall = Duration::ZERO;
    let mut total_cpu = Duration::ZERO;
    // Multiple windows require both workers to remain alive AND do new work;
    // merely exiting early or parking under the upper bound must not pass.
    for _ in 0..3 {
        alive(&mut child, &descendant);
        let before = cpu_used(job);
        let process_before = process_cpu_used(child.raw_handle().unwrap() as HANDLE)
            + process_cpu_used(descendant.as_raw_handle());
        let start = tokio::time::Instant::now();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let wall = start.elapsed();
        let cpu = cpu_used(job).saturating_sub(before);
        let process_cpu = (process_cpu_used(child.raw_handle().unwrap() as HANDLE)
            + process_cpu_used(descendant.as_raw_handle()))
        .saturating_sub(process_before);
        eprintln!("CPU window: wall={wall:?} Job={cpu:?} two fixtures={process_cpu:?}");
        alive(&mut child, &descendant);
        let next = progress(dir.path()).await;
        assert!(
            next[0] > previous[0] && next[1] > previous[1],
            "both busy workers must make progress in every window"
        );
        assert!(
            cpu > Duration::ZERO,
            "the job made no measurable CPU progress"
        );
        previous = next;
        total_wall += wall;
        total_cpu += cpu;
    }
    alive(&mut child, &descendant);
    end(job, &mut child).await;
    (total_wall, total_cpu)
}

#[tokio::test]
#[ignore = "native enforcement unqualified: CPU used 5.23s vs 0.86s allowance; console hosts exceed process cap; disposable Windows only"]
async fn cpu_rate_throttles_combined_descendants_not_lifetime_usage() {
    let (_, baseline) = measure_cpu(&Job::new().unwrap()).await;
    let mut requested = limits();
    requested.cpu_millis_per_second = 200;
    let (wall, used) = measure_cpu(&Job::limited(requested).unwrap()).await;
    let allowance = wall.mul_f64(0.2) + Duration::from_millis(250);
    assert!(
        baseline > allowance * 2,
        "machine was too busy to establish an uncapped control"
    );
    assert!(
        used > Duration::ZERO && used <= allowance,
        "combined CPU time {used:?} exceeded {allowance:?}"
    );
}
