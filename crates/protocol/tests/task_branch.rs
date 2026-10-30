// SPDX-License-Identifier: Apache-2.0
use protocol::{
    Command, ProjectId, Query, QueryData,
    model::{BranchListing, TaskBranch},
};
use serde_json::json;

#[test]
fn a_task_branch_is_optional_on_the_wire() {
    let legacy = json!({
        "command":"create_task", "project_id":ProjectId::new(), "kind":"coding",
        "title":null, "prompt":"Inspect the parser", "bundle":null,
        "harness":"claude_code", "model":null, "effort":null, "mission":null,
        "target_id":null, "placement":{"mode":"start_now"}
    });
    let command: Command = serde_json::from_value(legacy.clone()).unwrap();
    assert!(matches!(command, Command::CreateTask { branch: None, .. }));
    assert!(
        serde_json::to_value(command)
            .unwrap()
            .get("branch")
            .is_none()
    );
    // An existing branch leaves `new` off the wire.
    let mut chosen = legacy;
    chosen["branch"] = json!({"name": "feature/search"});
    let command: Command = serde_json::from_value(chosen.clone()).unwrap();
    let existing = TaskBranch {
        name: "feature/search".into(),
        new: false,
    };
    assert!(matches!(&command, Command::CreateTask { branch: Some(b), .. } if *b == existing));
    assert_eq!(
        serde_json::to_value(command).unwrap()["branch"],
        json!({"name": "feature/search"})
    );
    chosen["branch"] = json!({"name": "feature/search", "new": true});
    let command: Command = serde_json::from_value(chosen.clone()).unwrap();
    assert!(matches!(&command, Command::CreateTask { branch: Some(b), .. } if b.new));
    assert_eq!(serde_json::to_value(command).unwrap(), chosen);
}

#[test]
fn branch_listings_round_trip() {
    let project_id = ProjectId::new();
    let query: Query =
        serde_json::from_value(json!({"kind":"project_branches","project_id":project_id})).unwrap();
    assert_eq!(query, Query::ProjectBranches { project_id });
    let data = QueryData::ProjectBranches {
        listing: Some(BranchListing {
            current: None,
            branches: vec!["main".into()],
            dirty_files: 2,
            truncated: false,
        }),
    };
    let wire = serde_json::to_value(&data).unwrap();
    assert_eq!(
        wire,
        json!({"kind":"project_branches","listing":{"current":null,"branches":["main"],"dirty_files":2,"truncated":false}})
    );
    assert_eq!(serde_json::from_value::<QueryData>(wire).unwrap(), data);
}
