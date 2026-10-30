// SPDX-License-Identifier: Apache-2.0
mod common;
use common::*;
use report_pipeline::{budget::*, oscillation::*, schema::Area};
use std::path::Path;
fn ctx(run: &str, now_ms: i64) -> Context<'_> {
    Context {
        run_id: run,
        reporter_key: "fake-reporter",
        now_ms,
    }
}
fn allowed(b: &mut Budget, stage: Stage, c: Context<'_>) -> Ticket {
    match b.check(stage, c, &LocalOnly).unwrap() {
        Decision::Allowed(t) => t,
        Decision::Stop(r) => panic!("unexpected stop {r:?}"),
    }
}
fn stopped(b: &mut Budget, stage: Stage, c: Context<'_>, reason: StopReason) {
    assert!(matches!(b.check(stage,c,&LocalOnly).unwrap(),Decision::Stop(r) if r==reason));
}
fn open(path: &Path, config: BudgetConfig) -> Budget {
    Budget::open(path, config).unwrap()
}
#[test]
fn trusted_owner_wait_survives_restart_without_restoring_tokens_or_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let config = BudgetConfig {
        run_attempts: 1,
        run_wall_ms: 100,
        ..Default::default()
    };
    let mut b = open(&path, config.clone());
    let ticket = allowed(&mut b, Stage::Triage, ctx("run", 1));
    b.record(
        ticket,
        Usage {
            input_tokens: 100,
            output_tokens: 10,
        },
    )
    .unwrap();
    b.begin_wait("run", 20).unwrap();
    drop(b);
    let mut b = open(&path, config);
    let _ = allowed(&mut b, Stage::Approval, ctx("run", 10019));
    b.end_wait("run", 10020).unwrap();
    let _ = allowed(&mut b, Stage::Approval, ctx("run", 10021));
    stopped(&mut b, Stage::Fix, ctx("run", 10022), StopReason::Attempts);
    stopped(
        &mut b,
        Stage::Gates,
        ctx("run", 10101),
        StopReason::WallTime,
    );
}
#[test]
fn budget_reservations_survive_restart_and_refund_only_recorded_usage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("budget.db");
    let config = BudgetConfig {
        run_attempts: 1,
        ..BudgetConfig::default()
    };
    let mut b = open(&path, config.clone());
    let ticket = allowed(&mut b, Stage::Triage, ctx("run", 1));
    assert_eq!(ticket.input_limit(), 12_000);
    assert_eq!(ticket.output_limit(), 3_000);
    assert_eq!(ticket.deadline_ms(), 1 + 30 * 60 * 1000);
    drop(b);
    let mut b = open(&path, config);
    stopped(&mut b, Stage::Fix, ctx("run", 2), StopReason::Attempts);
    assert_eq!(
        b.record(
            ticket,
            Usage {
                input_tokens: 100,
                output_tokens: 10
            }
        )
        .unwrap(),
        None
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    let (i, o): (i64, i64) = db
        .query_row(
            "SELECT input,output FROM pipeline_runs WHERE id='run'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((i, o), (100, 10));
    assert_eq!(
        db.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
}
#[test]
fn daily_tokens_reporter_ci_and_release_budgets_reset_at_utc_day_boundary() {
    for (stage, config, reason) in [
        (
            Stage::Triage,
            BudgetConfig {
                daily_tokens: 15_000,
                ..BudgetConfig::default()
            },
            StopReason::DailyTokens,
        ),
        (
            Stage::Ci,
            BudgetConfig {
                daily_ci_runs: 1,
                ..BudgetConfig::default()
            },
            StopReason::CiRuns,
        ),
        (
            Stage::Release,
            BudgetConfig {
                daily_releases: 1,
                ..BudgetConfig::default()
            },
            StopReason::Releases,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut b = open(&dir.path().join("db"), config);
        let ticket = allowed(&mut b, stage, ctx("run", DAY_MS - 1));
        assert_eq!(ticket.deadline_ms(), DAY_MS);
        stopped(&mut b, stage, ctx("run", DAY_MS - 1), reason);
        let _ = allowed(&mut b, stage, ctx("run", DAY_MS));
    }
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("db");
    let config = BudgetConfig {
        daily_reporter_runs: 1,
        ..BudgetConfig::default()
    };
    let mut b = open(&p, config.clone());
    let _ = allowed(&mut b, Stage::Repro, ctx("one", DAY_MS - 1));
    stopped(
        &mut b,
        Stage::Repro,
        ctx("two", DAY_MS - 1),
        StopReason::ReporterRuns,
    );
    drop(b);
    let mut b = open(&p, config);
    let _ = allowed(&mut b, Stage::Repro, ctx("two", DAY_MS));
}
#[test]
fn budgets_stop_walltime_clock_rollback_and_input_output_exhaustion() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = open(
        &dir.path().join("db"),
        BudgetConfig {
            run_wall_ms: 10,
            ..BudgetConfig::default()
        },
    );
    let _ = allowed(&mut b, Stage::Gates, ctx("a", 10));
    stopped(
        &mut b,
        Stage::Gates,
        ctx("a", 9),
        StopReason::ClockMovedBack,
    );
    stopped(&mut b, Stage::Gates, ctx("a", 20), StopReason::WallTime);
    for config in [
        BudgetConfig {
            run_input_tokens: 11_999,
            ..BudgetConfig::default()
        },
        BudgetConfig {
            run_output_tokens: 2_999,
            ..BudgetConfig::default()
        },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut b = open(&dir.path().join("db"), config);
        stopped(&mut b, Stage::Triage, ctx("a", 0), StopReason::RunTokens);
    }
}
#[test]
fn pause_and_bans_are_persistent_and_remote_failure_stops() {
    struct RemotePause;
    impl PauseSource for RemotePause {
        fn paused(&self) -> report_pipeline::Result<bool> {
            Ok(true)
        }
    }
    struct Broken;
    impl PauseSource for Broken {
        fn paused(&self) -> report_pipeline::Result<bool> {
            Err(report_pipeline::Error::Invalid("unavailable"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("db");
    let mut b = open(&p, BudgetConfig::default());
    assert!(matches!(
        b.check(Stage::Gates, ctx("a", 0), &RemotePause).unwrap(),
        Decision::Stop(StopReason::Paused)
    ));
    assert!(matches!(
        b.check(Stage::Gates, ctx("a", 0), &Broken).unwrap(),
        Decision::Stop(StopReason::PauseUnavailable)
    ));
    b.set_local_pause(true).unwrap();
    drop(b);
    let mut b = open(&p, BudgetConfig::default());
    stopped(&mut b, Stage::Gates, ctx("a", 0), StopReason::Paused);
    b.set_local_pause(false).unwrap();
    b.ban("fake-reporter", "Repeated gate tampering").unwrap();
    stopped(&mut b, Stage::Gates, ctx("a", 0), StopReason::Banned);
    assert_eq!(
        b.ban_reason("fake-reporter").unwrap().as_deref(),
        Some("Repeated gate tampering")
    );
    b.unban("fake-reporter").unwrap();
    let _ = allowed(&mut b, Stage::Gates, ctx("a", 0));
}
#[test]
fn simultaneous_connections_cannot_double_reserve_and_overrun_pauses() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("db");
    let config = BudgetConfig {
        daily_tokens: 15_000,
        ..BudgetConfig::default()
    };
    let mut a = open(&p, config.clone());
    let mut b = open(&p, config);
    let t = allowed(&mut a, Stage::Triage, ctx("a", 0));
    stopped(&mut b, Stage::Fix, ctx("a", 1), StopReason::DailyTokens);
    assert_eq!(
        a.record(
            t,
            Usage {
                input_tokens: 12_001,
                output_tokens: 3000
            }
        )
        .unwrap(),
        Some(StopReason::ReservationExceeded)
    );
    stopped(&mut b, Stage::Gates, ctx("a", 1), StopReason::Paused);
}
#[test]
fn oscillation_detects_reversals_readds_and_same_area_different_signature() {
    let repo = Repo::new();
    repo.write("client/src/state/ordinary.ts", b"export const value = 2;\n");
    let first = repo.fixed();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut o = Oscillation::open(&path, DEFAULT_WINDOW_MS).unwrap();
    o.remember("REPORT_A", [1; 32], Area::Work, 1000, &first)
        .unwrap();
    drop(o);
    let o = Oscillation::open(&path, DEFAULT_WINDOW_MS).unwrap();
    repo.write("client/src/state/ordinary.ts", b"export const value = 1;\n");
    let fix = repo.commit();
    let reversal =
        report_pipeline::gates::CommitDiff::inspect(repo.path(), first.fix(), &fix).unwrap();
    let r = o
        .check("REPORT_B", [2; 32], Area::Work, 1001, &reversal)
        .unwrap();
    assert!(r.stop_for_owner());
    assert_eq!(r.conflicts[0].previous_report, "REPORT_A");
    assert_eq!(r.conflicts[0].new_report, "REPORT_B");
    let r = o
        .check(
            "REPORT_B",
            [2; 32],
            Area::Work,
            1000 + DEFAULT_WINDOW_MS + 1,
            &reversal,
        )
        .unwrap();
    assert!(!r.flagged());
    repo.write("client/src/state/ordinary.ts", b"export const value = 3;\n");
    let fix = repo.commit();
    let unrelated =
        report_pipeline::gates::CommitDiff::inspect(repo.path(), reversal.fix(), &fix).unwrap();
    // Re-adding unrelated content in another area must not be mistaken for a reversal.
    let other = Repo::new();
    other.write("client/src/state/extra.ts", b"const unrelated = 4;\n");
    let d = other.fixed();
    let r = o.check("REPORT_C", [2; 32], Area::Work, 1001, &d).unwrap();
    assert!(r.flagged());
    assert!(!r.stop_for_owner());
    assert!(
        !o.check("REPORT_C", [1; 32], Area::Work, 1001, &d)
            .unwrap()
            .flagged()
    );
    assert!(
        !o.check("REPORT_C", [2; 32], Area::Talk, 1001, &d)
            .unwrap()
            .flagged()
    );
    assert!(
        !o.check("REPORT_C", [2; 32], Area::Talk, 1001, &unrelated)
            .unwrap()
            .flagged()
    );
}
#[test]
fn oscillation_ignores_whitespace_only_and_structural_lines() {
    let repo = Repo::new();
    repo.write("client/src/state/ordinary.ts", b"}\n\n");
    let d = repo.fixed();
    let dir = tempfile::tempdir().unwrap();
    let mut o = Oscillation::open(&dir.path().join("db"), DEFAULT_WINDOW_MS).unwrap();
    o.remember("A", [1; 32], Area::Work, 0, &d).unwrap();
    repo.write("client/src/state/ordinary.ts", b"{\n\n");
    let fix = repo.commit();
    let d = report_pipeline::gates::CommitDiff::inspect(repo.path(), d.fix(), &fix).unwrap();
    assert!(
        !o.check("B", [2; 32], Area::Talk, 1, &d)
            .unwrap()
            .stop_for_owner()
    );
}
#[test]
fn checks_are_charged_once_per_submission_within_daily_caps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let config = BudgetConfig {
        daily_checks: 3,
        daily_reporter_checks: 2,
        ..Default::default()
    };
    let mut b = open(&path, config.clone());
    let wall = config.check_wall_ms;
    assert!(b.check_room(10).unwrap());
    assert_eq!(b.reserve_check("one", "alice", 10).unwrap(), Ok(10 + wall));
    // A retry after our own trouble isn't charged again.
    assert_eq!(b.reserve_check("one", "alice", 20).unwrap(), Ok(20 + wall));
    assert_eq!(b.reserve_check("two", "alice", 30).unwrap(), Ok(30 + wall));
    assert_eq!(
        b.reserve_check("three", "alice", 40).unwrap(),
        Err(StopReason::ReporterChecks)
    );
    assert_eq!(b.reserve_check("four", "bob", 50).unwrap(), Ok(50 + wall));
    assert!(!b.check_room(60).unwrap());
    assert_eq!(
        b.reserve_check("five", "carol", 60).unwrap(),
        Err(StopReason::Checks)
    );
    // Limits persist across restart and reset with the UTC day.
    drop(b);
    let mut b = open(&path, config);
    assert!(!b.check_room(70).unwrap());
    assert!(b.check_room(DAY_MS + 1).unwrap());
    assert_eq!(
        b.reserve_check("three", "alice", DAY_MS + 1).unwrap(),
        Ok(DAY_MS + 1 + wall)
    );
    b.ban("bob", "Repeated hostile pipeline signals").unwrap();
    assert_eq!(
        b.reserve_check("six", "bob", DAY_MS + 2).unwrap(),
        Err(StopReason::Banned)
    );
    b.set_local_pause(true).unwrap();
    assert!(!b.check_room(DAY_MS + 3).unwrap());
    assert_eq!(
        b.reserve_check("seven", "dave", DAY_MS + 3).unwrap(),
        Err(StopReason::Paused)
    );
    assert!(
        Budget::open(
            &dir.path().join("other"),
            BudgetConfig {
                check_wall_ms: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
}
