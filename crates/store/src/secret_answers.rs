// SPDX-License-Identifier: Apache-2.0
//! Scrub answers retained by versions that only hid the password input.
use super::*;

pub(super) fn scrub(connection: &mut Connection) -> Result<()> {
    // Do not make a backup containing the very values being removed. Keep
    // sequence numbers and command identities: recovery must never redeliver.
    connection.pragma_update(None, "secure_delete", "ON")?;
    let tx = connection.transaction()?;
    tx.execute_batch(
        "CREATE TEMP TABLE secret_approvals (id TEXT PRIMARY KEY);
         INSERT INTO secret_approvals
         SELECT id FROM documents WHERE kind='approval'
           AND EXISTS (SELECT 1 FROM json_each(documents.json, '$.questions') q
                       WHERE json_extract(q.value, '$.secret')=1);
         UPDATE commands SET json=json_set(json,
             '$.envelope.body.decision.text', '',
             '$.envelope.body.decision.answers', json('[]'))
         WHERE json_valid(json) AND json_extract(json,'$.envelope.body.command')='resolve_approval'
           AND json_extract(json,'$.envelope.body.decision.decision')='answer'
           AND (json_extract(json,'$.envelope.body.approval_id') IN (SELECT id FROM secret_approvals)
                OR NOT EXISTS (SELECT 1 FROM documents a WHERE a.kind='approval'
                   AND a.id=json_extract(commands.json,'$.envelope.body.approval_id')));
         UPDATE documents SET json=json_set(json,
             '$.delivery.decision.text', '', '$.delivery.decision.answers', json('[]'))
         WHERE kind='approval' AND id IN (SELECT id FROM secret_approvals)
           AND json_extract(json,'$.delivery.decision.decision')='answer';
         UPDATE journal SET json=json_set(json,
             '$.event.approval.delivery.decision.text', '',
             '$.event.approval.delivery.decision.answers', json('[]'))
         WHERE json_valid(json) AND json_extract(json,'$.event.type')='approval_upserted'
           AND json_extract(json,'$.event.approval.delivery.decision.decision')='answer'
           AND (json_extract(json,'$.event.approval.approval_id') IN (SELECT id FROM secret_approvals)
                OR EXISTS (SELECT 1 FROM json_each(journal.json, '$.event.approval.questions') q
                           WHERE json_extract(q.value, '$.secret')=1));
         UPDATE commands SET json=json_set(json, '$.envelope.body.text',
             'The private answer was removed. Ask for it again in a fresh private question.')
         WHERE json_valid(json)
           AND json_extract(json,'$.envelope.body.command') IN ('resume_task','queue_continuation')
           AND id IN (SELECT json_extract(json,'$.command_id') FROM documents
                      WHERE kind='task_followup'
                        AND id IN (SELECT 'answer:' || id FROM secret_approvals));
         UPDATE documents SET json=json_set(json, '$.text',
             'The private answer was removed. Ask for it again in a fresh private question.')
         WHERE kind='task_followup' AND id IN (SELECT 'answer:' || id FROM secret_approvals);
         DROP TABLE secret_approvals;",
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opening_scrubs_legacy_answers_without_recycling_replay_or_command_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answers.sqlite");
        drop(Store::open(&path, Retention::default()).unwrap());
        let db = Connection::open(&path).unwrap();
        let answer = json!({"decision":"answer", "text":"issue91-password-sentinel",
            "answers":[{"question":"Password?","choices":["issue91-password-sentinel"]}]});
        for (id, secret) in [("private", true), ("public", false)] {
            let approval = json!({"approval_id":id,"questions":[{"question":"Password?","secret":secret}],
                "delivery":{"decision":answer,"at":42,"error":null}});
            db.execute(
                "INSERT INTO documents VALUES('approval',?1,?2)",
                params![id, approval.to_string()],
            )
            .unwrap();
            let event = json!({"seq":if secret {1} else {2},"event":{"type":"approval_upserted","approval":approval}});
            db.execute(
                "INSERT INTO journal(at,json) VALUES(42,?1)",
                [event.to_string()],
            )
            .unwrap();
            let command = json!({"envelope":{"command_id":id,"body":{"command":"resolve_approval","approval_id":id,"decision":answer}},
                "stage":"uncertain","pending":true});
            db.execute(
                "INSERT INTO commands VALUES(?1,?2)",
                params![id, command.to_string()],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO documents VALUES('task_followup','answer:private',?1)",
            [json!({"text":"issue91-password-sentinel","command_id":"keep-me"}).to_string()],
        )
        .unwrap();
        // An unreadable command must keep its existing quarantine/recovery path.
        db.execute("INSERT INTO commands VALUES('broken','invalid json')", [])
            .unwrap();
        drop(db);
        for _ in 0..2 {
            let store = Store::open(&path, Retention::default()).unwrap();
            let db = &store.connection;
            for (table, filter) in [
                ("commands", "id='private'"),
                ("documents", "id IN ('private','answer:private')"),
                ("journal", "seq=1"),
            ] {
                let sql = format!("SELECT json FROM {table} WHERE {filter}");
                let rows = db
                    .prepare(&sql)
                    .unwrap()
                    .query_map([], |row| row.get::<_, String>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                assert!(!rows.is_empty());
                assert!(
                    rows.iter()
                        .all(|value| !value.contains("issue91-password-sentinel"))
                );
            }
            let command: String = db
                .query_row("SELECT json FROM commands WHERE id='private'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let command: serde_json::Value = serde_json::from_str(&command).unwrap();
            assert_eq!(command["stage"], "uncertain");
            assert_eq!(command["pending"], true);
            let public: String = db
                .query_row("SELECT json FROM commands WHERE id='public'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(public.contains("issue91-password-sentinel"));
            let ids: String = db
                .query_row("SELECT group_concat(seq) FROM journal", [], |r| r.get(0))
                .unwrap();
            assert_eq!(ids, "1,2");
        }
    }
}
