// SPDX-License-Identifier: Apache-2.0
//! Checks add detail to Working rather than introducing an unknown task state.
use protocol::{Command, Query, checks::*, model::Task};
use serde::Deserialize;
use serde_json::{Value, json};

fn old_task() -> Value {
    json!({"task_id":"018f0000-0000-7000-8000-000000000001","project_id":"018f0000-0000-7000-8000-000000000002","kind":"coding","title":"Improve the app","state":"working","revision":1,"harness":"claude_code","model":null,"effort":null,"created_by":{"kind":"local_owner"},"created_at":1,"updated_at":1,"current_run":null,"target_id":null,"mission":null,"attention_count":0,"status_line":null})
}

#[test]
fn older_tasks_without_checks_still_decode_and_omit_progress() {
    let task: Task = serde_json::from_value(old_task()).unwrap();
    assert!(task.checking.is_none());
    assert!(
        serde_json::to_value(task)
            .unwrap()
            .get("checking")
            .is_none()
    );
}

#[test]
fn an_old_phone_lists_and_receives_alerts_for_a_working_checking_task() {
    #[derive(Deserialize)]
    struct OldPhoneTask {
        task_id: String,
        state: String,
        title: String,
    }
    let mut value = old_task();
    value["checking"] =
        json!({"job_id":"018f0000-0000-7000-8000-000000000003","started_at":10,"done":1,"total":4});
    let task: Task = serde_json::from_value(value).unwrap();
    let encoded = serde_json::to_value(&task).unwrap();
    let listed: OldPhoneTask = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(listed.state, "working");
    assert_eq!(listed.title, "Improve the app");
    let alert = serde_json::to_value(protocol::Event::TaskUpserted { task: task.clone() }).unwrap();
    let alerted: OldPhoneTask = serde_json::from_value(alert["task"].clone()).unwrap();
    assert_eq!(alerted.state, "working");
    let stop: Command = serde_json::from_value(
        json!({"command":"stop_task", "task_id":listed.task_id,"force":false}),
    )
    .unwrap();
    assert!(matches!(stop, Command::StopTask { task_id, .. } if task_id == task.task_id));
}

#[test]
fn check_defaults_and_requests_round_trip_without_changing_agent_reports() {
    let check: Check = serde_json::from_value(json!({"check_id":"018f0000-0000-7000-8000-000000000003","name":"App builds correctly","about":"Makes sure the app can be built.","run":"npm run build"})).unwrap();
    assert_eq!(check.timeout_secs, 600);
    assert!(!check.network);
    assert!(check.enabled);
    assert_eq!(check.when, CheckWhen::EveryTask);
    assert!(check.typical_secs.is_none());
    let query = Query::ChecksList { project_id: None };
    assert_eq!(
        serde_json::from_value::<Query>(serde_json::to_value(&query).unwrap()).unwrap(),
        query
    );
    assert_eq!(
        <protocol::model::CheckResult as ts_rs::TS>::name(&ts_rs::Config::default()),
        "CheckResult"
    );
    assert_eq!(
        <CheckResult as ts_rs::TS>::name(&ts_rs::Config::default()),
        "OphioCheckResult"
    );
}

#[test]
fn uncertain_job_resolution_is_an_explicit_optional_person_confirmation() {
    let job_id = protocol::CheckJobId::new();
    let plain = json!({"command":"checks_stop_job", "job_id":job_id});
    let command: Command = serde_json::from_value(plain.clone()).unwrap();
    assert!(matches!(
        command,
        Command::ChecksStopJob {
            confirmed_stopped: false,
            ..
        }
    ));
    assert_eq!(serde_json::to_value(command).unwrap(), plain);
    let confirmed = json!({"command":"checks_stop_job", "job_id":job_id, "confirmed_stopped":true});
    let command: Command = serde_json::from_value(confirmed.clone()).unwrap();
    assert!(matches!(
        command,
        Command::ChecksStopJob {
            confirmed_stopped: true,
            ..
        }
    ));
    assert_eq!(serde_json::to_value(command).unwrap(), confirmed);
}

fn old_project_checks_view() -> Value {
    json!({
        "project_id":"018f0000-0000-7000-8000-000000000002",
        "project_name":"App",
        "approved": {
            "project_id":"018f0000-0000-7000-8000-000000000002",
            "version":0,"checks":[],"input_fingerprints":[],"changed_at":1,
            "changed_by":{"kind":"person","device_id":null}
        },
        "checks":[],"drafts":[]
    })
}

#[test]
fn project_checks_without_an_unfinished_job_round_trip_without_adding_a_field() {
    let value = old_project_checks_view();
    let view: ProjectChecksView = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(view).unwrap(), value);
}

#[test]
fn project_checks_with_an_unfinished_job_round_trip() {
    let mut value = old_project_checks_view();
    value["unfinished_job"] = json!({
        "id":"018f0000-0000-7000-8000-000000000003",
        "project_id":value["project_id"],"task_id":null,"run_id":null,
        "purpose":{"kind":"run_now","check_ids":[]},"checks_version":0,
        "checks":[],"state":"uncertain","process":null,"created_at":1,
        "started_at":null,"finished_at":null,"sandbox":null,"results":[],
        "message":"Check whether the process stopped."
    });
    let view: ProjectChecksView = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(view).unwrap(), value);
}
