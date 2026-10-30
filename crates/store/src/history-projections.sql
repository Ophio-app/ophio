-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE history_runs (id TEXT PRIMARY KEY, task_id TEXT NOT NULL, open INTEGER NOT NULL, json TEXT NOT NULL);
CREATE INDEX history_runs_task ON history_runs(task_id,id);
CREATE INDEX history_runs_open ON history_runs(task_id) WHERE open=1;

-- Keep only unresolved notices without a stored task. An anti-join over all
-- unresolved notices on each admission would still scan finished history.
CREATE TABLE history_orphan_attention (id TEXT PRIMARY KEY, task_id TEXT);
CREATE INDEX history_orphan_task ON history_orphan_attention(task_id);
INSERT INTO history_orphan_attention
SELECT id,json_extract(json,'$.task_id') FROM documents AS a
WHERE kind='attention' AND json_extract(json,'$.resolved_at') IS NULL
AND NOT EXISTS(SELECT 1 FROM documents AS t WHERE t.kind='task' AND t.id=json_extract(a.json,'$.task_id'));

CREATE TRIGGER history_attention_insert AFTER INSERT ON documents WHEN new.kind='attention' BEGIN
    INSERT INTO history_orphan_attention
    SELECT new.id,json_extract(new.json,'$.task_id')
    WHERE json_extract(new.json,'$.resolved_at') IS NULL
    AND NOT EXISTS(SELECT 1 FROM documents WHERE kind='task' AND id=json_extract(new.json,'$.task_id'));
END;
CREATE TRIGGER history_attention_update AFTER UPDATE ON documents WHEN old.kind='attention' OR new.kind='attention' BEGIN
    DELETE FROM history_orphan_attention WHERE id=old.id;
    INSERT INTO history_orphan_attention
    SELECT new.id,json_extract(new.json,'$.task_id')
    WHERE new.kind='attention' AND json_extract(new.json,'$.resolved_at') IS NULL
    AND NOT EXISTS(SELECT 1 FROM documents WHERE kind='task' AND id=json_extract(new.json,'$.task_id'));
END;
CREATE TRIGGER history_attention_delete AFTER DELETE ON documents WHEN old.kind='attention' BEGIN
    DELETE FROM history_orphan_attention WHERE id=old.id;
END;
CREATE TRIGGER history_task_insert AFTER INSERT ON documents WHEN new.kind='task' BEGIN
    DELETE FROM history_orphan_attention WHERE task_id=new.id;
END;
CREATE TRIGGER history_task_delete AFTER DELETE ON documents WHEN old.kind='task' BEGIN
    INSERT INTO history_orphan_attention
    SELECT id,json_extract(json,'$.task_id') FROM documents INDEXED BY attention_history_unresolved
    WHERE kind='attention' AND json_extract(json,'$.resolved_at') IS NULL AND json_extract(json,'$.task_id')=old.id;
END;
CREATE TRIGGER history_task_rekey AFTER UPDATE OF kind,id ON documents WHEN old.kind='task' OR new.kind='task' BEGIN
    INSERT INTO history_orphan_attention
    SELECT id,json_extract(json,'$.task_id') FROM documents INDEXED BY attention_history_unresolved
    WHERE old.kind='task' AND (new.kind!='task' OR new.id!=old.id)
    AND kind='attention' AND json_extract(json,'$.resolved_at') IS NULL AND json_extract(json,'$.task_id')=old.id;
    DELETE FROM history_orphan_attention WHERE new.kind='task' AND task_id=new.id;
END;
