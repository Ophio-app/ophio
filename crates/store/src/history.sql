-- SPDX-License-Identifier: Apache-2.0
CREATE INDEX IF NOT EXISTS task_history_page ON documents(json_extract(json,'$.project_id'),json_extract(json,'$.updated_at') DESC,id DESC) WHERE kind='task';
CREATE INDEX IF NOT EXISTS task_history_snapshot ON documents(json_extract(json,'$.updated_at') DESC,id DESC) WHERE kind='task';
CREATE INDEX IF NOT EXISTS task_history_live ON documents(id) WHERE kind='task' AND json_extract(json,'$.state') IN ('accepted','dispatching','working','awaiting_decision');
CREATE INDEX IF NOT EXISTS approval_history_task ON documents(json_extract(json,'$.task_id'),id) WHERE kind='approval';
CREATE INDEX IF NOT EXISTS approval_history_live ON documents(json_extract(json,'$.task_id')) WHERE kind='approval' AND json_extract(json,'$.state')='pending';
CREATE INDEX IF NOT EXISTS attention_history_live ON documents(json_extract(json,'$.task_id')) WHERE kind='attention' AND json_extract(json,'$.resolved_at') IS NULL AND json_extract(json,'$.kind') NOT IN ('completed','failure');
CREATE INDEX IF NOT EXISTS attention_history_unresolved ON documents(json_extract(json,'$.task_id')) WHERE kind='attention' AND json_extract(json,'$.resolved_at') IS NULL;
CREATE INDEX IF NOT EXISTS attention_history_approval ON documents(json_extract(json,'$.task_id'),json_extract(json,'$.approval_id'),json_extract(json,'$.created_at') DESC,id) WHERE kind='attention';
