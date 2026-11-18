// SPDX-License-Identifier: Apache-2.0
//! Approved checks, their history and agent drafts are separate documents.
//! Only person-approved mutations write the definitions used by check jobs.

use crate::{Read, Result, Write};
use cap_std::fs::{Dir, OpenOptions};
use globset::Glob;
use protocol::{
    CheckDraftId, CheckId, CheckJobId, DeviceId, ErrorCode, ProjectId, ProtocolError, TaskId,
    checks::*, time::Timestamp,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::Read as _,
    path::{Component, Path},
};

pub const APPROVED: &str = "project_checks";
pub const DRAFTS: &str = "checks_draft";
pub const JOBS: &str = "check_job";
const HISTORY_LIMIT: usize = 20;
const CHECK_LIMIT: usize = 64;
const INPUT_BYTE_LIMIT: u64 = 16 * 1024 * 1024;

#[cfg(test)]
#[path = "checks_tests.rs"]
mod tests;

pub(crate) fn install(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS checks_draft_project ON documents(json_extract(json,'$.project_id')) WHERE kind='checks_draft';
         CREATE INDEX IF NOT EXISTS checks_draft_task ON documents(json_extract(json,'$.task_id')) WHERE kind='checks_draft';
         CREATE INDEX IF NOT EXISTS check_job_project ON documents(json_extract(json,'$.project_id')) WHERE kind='check_job';
         CREATE INDEX IF NOT EXISTS check_job_task ON documents(json_extract(json,'$.task_id')) WHERE kind='check_job';
         CREATE INDEX IF NOT EXISTS check_job_state ON documents(json_extract(json,'$.state')) WHERE kind='check_job';"
    )?;
    Ok(())
}

/// An omitted request is valid for setup. Validate before treating whitespace
/// as absent so oversized or unsupported input cannot evade admission checks.
/// Also accepts blank requests persisted by older setup tasks.
pub fn normalize_checks_request(
    request: &str,
) -> std::result::Result<Option<String>, ProtocolError> {
    if request.len() > 16_384
        || request
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "Use a request of at most 16384 UTF-8 bytes, without unsupported control characters.",
        ));
    }
    Ok((!request.trim().is_empty()).then(|| request.to_owned()))
}

fn rule(code: ErrorCode, message: &str) -> crate::Error {
    ProtocolError::new(code, message).into()
}
fn invalid(message: &str) -> crate::Error {
    rule(ErrorCode::InvalidRequest, message)
}
fn stale() -> crate::Error {
    rule(
        ErrorCode::StaleRevision,
        "These checks changed since you opened them. Refresh them, or ask your agent to suggest the change again.",
    )
}
fn history_kind(project_id: ProjectId) -> String {
    format!("checks_history:{project_id}")
}
fn item_id(item: &CheckDraftItem) -> CheckId {
    match item {
        CheckDraftItem::Add { check, .. } | CheckDraftItem::Change { check, .. } => check.check_id,
        CheckDraftItem::Remove { check_id, .. } => *check_id,
    }
}
fn open_draft(state: ChecksDraftState) -> bool {
    matches!(
        state,
        ChecksDraftState::Proving | ChecksDraftState::Ready | ChecksDraftState::Stale
    )
}

/// A project with no approved checks has revision zero. Merely reading it does
/// not create a document or change how projects without checks behave.
pub fn empty_checks(project_id: ProjectId) -> ProjectChecks {
    ProjectChecks {
        project_id,
        version: 0,
        checks: vec![],
        input_fingerprints: vec![],
        changed_at: Timestamp(0),
        changed_by: ChecksChangedBy::Person { device_id: None },
    }
}

impl Read<'_> {
    pub fn project_checks(&self, project_id: ProjectId) -> Result<Option<ProjectChecks>> {
        self.get(APPROVED, project_id)
    }
    pub fn all_project_checks(&self) -> Result<Vec<ProjectChecks>> {
        self.list(APPROVED)
    }
    /// Newest first, including the current version, independent of journal retention.
    pub fn checks_history(&self, project_id: ProjectId) -> Result<Vec<ProjectChecks>> {
        let mut versions: Vec<ProjectChecks> = self.list(&history_kind(project_id))?;
        versions.sort_by_key(|v| std::cmp::Reverse(v.version));
        Ok(versions)
    }
    pub fn checks_draft(&self, draft_id: CheckDraftId) -> Result<Option<ChecksDraft>> {
        self.get(DRAFTS, draft_id)
    }
    pub fn checks_drafts(&self, project_id: ProjectId) -> Result<Vec<ChecksDraft>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT json FROM documents WHERE kind='checks_draft' AND json_extract(json,'$.project_id')=?1 ORDER BY id"
        )?;
        statement
            .query_map([project_id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| crate::decode(&row?))
            .collect()
    }
    pub fn task_checks_draft(&self, task_id: TaskId) -> Result<Option<ChecksDraft>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT json FROM documents WHERE kind='checks_draft' AND json_extract(json,'$.task_id')=?1 AND json_extract(json,'$.state') IN ('proving','ready','stale') ORDER BY id DESC LIMIT 1"
        )?;
        use rusqlite::OptionalExtension;
        let json: Option<String> = statement
            .query_row([task_id.to_string()], |row| row.get(0))
            .optional()?;
        json.map(|value| crate::decode(&value)).transpose()
    }
    pub fn check_job(&self, job_id: CheckJobId) -> Result<Option<CheckJob>> {
        self.get(JOBS, job_id)
    }
    /// Includes completed jobs for receipts. The newest job comes first.
    pub fn project_check_jobs(&self, project_id: ProjectId) -> Result<Vec<CheckJob>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT json FROM documents WHERE kind='check_job' AND json_extract(json,'$.project_id')=?1 ORDER BY id DESC"
        )?;
        statement
            .query_map([project_id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| crate::decode(&row?))
            .collect()
    }
    pub fn task_check_jobs(&self, task_id: TaskId) -> Result<Vec<CheckJob>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT json FROM documents WHERE kind='check_job' AND json_extract(json,'$.task_id')=?1 ORDER BY id DESC"
        )?;
        statement
            .query_map([task_id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| crate::decode(&row?))
            .collect()
    }
    /// Restart reconciliation must also inspect uncertain jobs; never silently
    /// treat a process whose identity is unknown as having finished.
    pub fn pending_check_jobs(&self) -> Result<Vec<CheckJob>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT json FROM documents WHERE kind='check_job' AND json_extract(json,'$.state') IN ('pending','running','cancelling','uncertain') ORDER BY id"
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|row| crate::decode(&row?))
            .collect()
    }
}

impl Write<'_> {
    /// Replace only this task's open proposal, never the approved checks. A new
    /// identity prevents an older proof job from completing the replacement.
    pub fn propose_checks(
        &mut self,
        project_id: ProjectId,
        base_version: u64,
        task_id: Option<TaskId>,
        request: Option<String>,
        mut items: Vec<CheckDraftItem>,
        project_root: &Path,
    ) -> Result<ChecksDraft> {
        let current = self.checks_at_base(project_id, base_version)?;
        if items.is_empty() || items.len() > CHECK_LIMIT {
            return Err(invalid("Suggest between 1 and 64 changes to the checks."));
        }
        let request = request
            .as_deref()
            .map(normalize_checks_request)
            .transpose()?
            .flatten();
        let mut ids = HashSet::new();
        for item in &mut items {
            if !ids.insert(item_id(item)) {
                return Err(invalid("Suggest only one change for each check."));
            }
            match item {
                CheckDraftItem::Add { check, note } => {
                    if current
                        .checks
                        .iter()
                        .any(|old| old.check_id == check.check_id)
                    {
                        return Err(invalid(
                            "This check already exists. Suggest changing it instead.",
                        ));
                    }
                    validate_text(
                        note,
                        2_000,
                        "Describe the suggested change using 1 to 2000 UTF-8 bytes, without control characters.",
                        false,
                    )?;
                    check.typical_secs = None;
                    validate_check(check)?;
                    fingerprint_check(project_root, check)?;
                }
                CheckDraftItem::Change { check, note } => {
                    let old = current
                        .checks
                        .iter()
                        .find(|old| old.check_id == check.check_id)
                        .ok_or_else(|| {
                            rule(
                                ErrorCode::NotFound,
                                "The check you want to change no longer exists.",
                            )
                        })?;
                    validate_text(
                        note,
                        2_000,
                        "Describe the suggested change using 1 to 2000 UTF-8 bytes, without control characters.",
                        false,
                    )?;
                    keep_measurement(check, Some(old));
                    validate_check(check)?;
                    fingerprint_check(project_root, check)?;
                }
                CheckDraftItem::Remove { check_id, reason } => {
                    require_check(&current.checks, *check_id)?;
                    validate_text(
                        reason,
                        2_000,
                        "Say why this check should be removed using 1 to 2000 UTF-8 bytes, without control characters.",
                        false,
                    )?;
                }
            }
        }
        let mut proposed = current.checks.clone();
        for item in &items {
            apply_item(&mut proposed, item);
        }
        validate_checks(&proposed)?;
        if let Some(task_id) = task_id
            && let Some(mut old) = self.read().task_checks_draft(task_id)?
        {
            if old.project_id != project_id {
                return Err(invalid(
                    "The task's suggested checks belong to a different project.",
                ));
            }
            old.state = ChecksDraftState::Discarded;
            old.updated_at = self.now;
            self.put(DRAFTS, old.id, &old)?;
        }
        let draft = ChecksDraft {
            id: CheckDraftId::new(),
            project_id,
            base_version,
            task_id,
            request,
            items,
            proofs: vec![],
            state: ChecksDraftState::Proving,
            created_at: self.now,
            updated_at: self.now,
        };
        self.put(DRAFTS, draft.id, &draft)?;
        Ok(draft)
    }

    /// Proofs are host results, not supplied through the proposal API. Even a
    /// failing proof can be approved once the person has seen its result.
    pub fn finish_checks_proof(
        &mut self,
        draft_id: CheckDraftId,
        proofs: Vec<CheckResult>,
    ) -> Result<ChecksDraft> {
        let mut draft: ChecksDraft = self.read().require(DRAFTS, draft_id)?;
        if !matches!(
            draft.state,
            ChecksDraftState::Proving | ChecksDraftState::Stale
        ) {
            return Err(rule(
                ErrorCode::AlreadyResolved,
                "This suggestion is no longer waiting for its checks to run.",
            ));
        }
        let expected: HashSet<_> = draft
            .items
            .iter()
            .filter_map(|item| match item {
                CheckDraftItem::Add { check, .. } | CheckDraftItem::Change { check, .. } => {
                    Some(check.check_id)
                }
                CheckDraftItem::Remove { .. } => None,
            })
            .collect();
        let mut seen = HashSet::new();
        for result in &proofs {
            validate_result(result)?;
            if !expected.contains(&result.check_id) || !seen.insert(result.check_id) {
                return Err(invalid(
                    "The proof results do not match the suggested checks.",
                ));
            }
        }
        if seen != expected {
            return Err(invalid("Some suggested checks still need a proof result."));
        }
        let current = self
            .read()
            .project_checks(draft.project_id)?
            .unwrap_or_else(|| empty_checks(draft.project_id));
        draft.state = if draft.base_version == current.version {
            ChecksDraftState::Ready
        } else {
            ChecksDraftState::Stale
        };
        draft.proofs = proofs;
        draft.updated_at = self.now;
        self.put(DRAFTS, draft.id, &draft)?;
        Ok(draft)
    }

    pub fn change_checks(
        &mut self,
        project_id: ProjectId,
        base_version: u64,
        edit: CheckEdit,
        device_id: Option<DeviceId>,
        project_root: &Path,
    ) -> Result<ProjectChecks> {
        let current = self.checks_at_base(project_id, base_version)?;
        let mut next = current.clone();
        match edit {
            CheckEdit::SetEnabled { check_id, enabled } => {
                find_check_mut(&mut next.checks, check_id)?.enabled = enabled;
            }
            CheckEdit::Delete { check_id } => {
                require_check(&next.checks, check_id)?;
                next.checks.retain(|check| check.check_id != check_id);
                next.input_fingerprints
                    .retain(|value| value.check_id != check_id);
            }
            CheckEdit::Save { mut check } => {
                let previous = current
                    .checks
                    .iter()
                    .find(|old| old.check_id == check.check_id);
                keep_measurement(&mut check, previous);
                validate_check(&check)?;
                let fingerprints = fingerprint_check(project_root, &check)?;
                next.input_fingerprints
                    .retain(|value| value.check_id != check.check_id);
                next.input_fingerprints.extend(fingerprints);
                upsert_check(&mut next.checks, &check);
            }
            CheckEdit::Undo => {
                if current.version == 0 {
                    return Err(invalid("There is no earlier change to restore."));
                }
                let previous = if current.version == 1 {
                    empty_checks(project_id)
                } else {
                    self.read()
                        .get(&history_kind(project_id), current.version - 1)?
                        .ok_or_else(|| {
                            rule(
                                ErrorCode::NotFound,
                                "The earlier checks are no longer in the saved history.",
                            )
                        })?
                };
                next.checks = previous.checks;
                next.input_fingerprints = previous.input_fingerprints;
            }
            CheckEdit::KeepInputs { check_id } => {
                let check = require_check(&next.checks, check_id)?;
                let fingerprints = fingerprint_check(project_root, check)?;
                next.input_fingerprints
                    .retain(|value| value.check_id != check_id);
                next.input_fingerprints.extend(fingerprints);
            }
        }
        self.approve_checks(next, ChecksChangedBy::Person { device_id })
    }

    pub fn apply_checks_draft(
        &mut self,
        draft_id: CheckDraftId,
        keep: &[CheckId],
        device_id: Option<DeviceId>,
        project_root: &Path,
    ) -> Result<ProjectChecks> {
        let mut draft: ChecksDraft = self.read().require(DRAFTS, draft_id)?;
        match draft.state {
            ChecksDraftState::Stale => return Err(stale()),
            ChecksDraftState::Proving => {
                return Err(rule(
                    ErrorCode::Conflict,
                    "Ophio is still running these suggested checks. Wait for the results before using them.",
                ));
            }
            ChecksDraftState::Applied | ChecksDraftState::Discarded => {
                return Err(rule(
                    ErrorCode::AlreadyResolved,
                    "This suggestion has already been used or discarded.",
                ));
            }
            ChecksDraftState::Ready => {}
        }
        let mut next = self.checks_at_base(draft.project_id, draft.base_version)?;
        let ids: HashSet<_> = keep.iter().copied().collect();
        if ids.is_empty() || ids.len() != keep.len() {
            return Err(invalid(
                "Choose at least one suggested change, or discard the suggestion.",
            ));
        }
        if ids
            .iter()
            .any(|id| !draft.items.iter().any(|item| item_id(item) == *id))
        {
            return Err(invalid("A selected change is not part of this suggestion."));
        }
        for item in draft
            .items
            .iter()
            .filter(|item| ids.contains(&item_id(item)))
        {
            let id = item_id(item);
            let fingerprints = match item {
                CheckDraftItem::Add { check, .. } | CheckDraftItem::Change { check, .. } => {
                    validate_check(check)?;
                    fingerprint_check(project_root, check)?
                }
                CheckDraftItem::Remove { .. } => vec![],
            };
            next.input_fingerprints.retain(|value| value.check_id != id);
            next.input_fingerprints.extend(fingerprints);
            apply_item(&mut next.checks, item);
        }
        draft.state = ChecksDraftState::Applied;
        draft.updated_at = self.now;
        self.put(DRAFTS, draft.id, &draft)?;
        self.approve_checks(
            next,
            ChecksChangedBy::Draft {
                draft_id,
                device_id,
            },
        )
    }

    pub fn discard_checks_draft(&mut self, draft_id: CheckDraftId) -> Result<ChecksDraft> {
        let mut draft: ChecksDraft = self.read().require(DRAFTS, draft_id)?;
        if draft.state == ChecksDraftState::Applied {
            return Err(rule(
                ErrorCode::AlreadyResolved,
                "This suggestion has already been used. Undo the change instead.",
            ));
        }
        draft.state = ChecksDraftState::Discarded;
        draft.updated_at = self.now;
        self.put(DRAFTS, draft.id, &draft)?;
        Ok(draft)
    }

    /// The service owns admission and state transitions, including the project
    /// writer reservation, and saves them in the same store transaction.
    pub fn put_check_job(&mut self, job: &CheckJob) -> Result<()> {
        if job.checks_version > protocol::MAX_SAFE_INTEGER {
            return Err(crate::Error::IntegerExhausted);
        }
        validate_checks(&job.checks)?;
        let ids: HashSet<_> = job.checks.iter().map(|check| check.check_id).collect();
        let mut seen = HashSet::new();
        for result in &job.results {
            validate_result(result)?;
            if !ids.contains(&result.check_id) || !seen.insert(result.check_id) {
                return Err(invalid("The results do not match the checks in this run."));
            }
        }
        if let Some(old) = self.read().check_job(job.id)?
            && (old.project_id != job.project_id
                || old.task_id != job.task_id
                || old.run_id != job.run_id
                || old.purpose != job.purpose
                || old.checks_version != job.checks_version
                || old.checks != job.checks
                || old.created_at != job.created_at)
        {
            return Err(invalid(
                "The checks belonging to an existing run cannot be replaced.",
            ));
        }
        self.put(JOBS, job.id, job)
    }

    /// Record host-measured times only from a completed approved-check job.
    /// Reconciliation may repeat this call, so the latest passing duration is
    /// used directly instead of folding the same result into an average twice.
    pub fn record_check_durations(&mut self, job_id: CheckJobId) -> Result<()> {
        let job: CheckJob = self.read().require(JOBS, job_id)?;
        if job.state != CheckJobState::Finished
            || matches!(job.purpose, CheckJobPurpose::DraftProof { .. })
        {
            return Ok(());
        }
        let Some(mut approved) = self.read().project_checks(job.project_id)? else {
            return Ok(());
        };
        if approved.version != job.checks_version {
            return Ok(());
        }
        let mut changed = false;
        for result in &job.results {
            if result.outcome != CheckOutcome::Passed {
                continue;
            }
            validate_result(result)?;
            let Some(frozen) = job
                .checks
                .iter()
                .find(|check| check.check_id == result.check_id)
            else {
                continue;
            };
            let Some(current) = approved
                .checks
                .iter_mut()
                .find(|check| check.check_id == result.check_id)
            else {
                continue;
            };
            let mut comparable = frozen.clone();
            comparable.typical_secs = current.typical_secs;
            if comparable != *current || current.typical_secs == Some(result.duration_secs) {
                continue;
            }
            current.typical_secs = Some(result.duration_secs);
            changed = true;
        }
        if changed {
            self.put(APPROVED, approved.project_id, &approved)?;
            self.put(
                &history_kind(approved.project_id),
                approved.version,
                &approved,
            )?;
        }
        Ok(())
    }

    fn checks_at_base(&self, project_id: ProjectId, version: u64) -> Result<ProjectChecks> {
        let current = self
            .read()
            .project_checks(project_id)?
            .unwrap_or_else(|| empty_checks(project_id));
        if current.version != version {
            return Err(stale());
        }
        Ok(current)
    }
    fn approve_checks(
        &mut self,
        mut next: ProjectChecks,
        changed_by: ChecksChangedBy,
    ) -> Result<ProjectChecks> {
        validate_checks(&next.checks)?;
        next.version = next
            .version
            .checked_add(1)
            .filter(|value| *value <= protocol::MAX_SAFE_INTEGER)
            .ok_or(crate::Error::IntegerExhausted)?;
        next.changed_at = self.now;
        next.changed_by = changed_by;
        self.put(APPROVED, next.project_id, &next)?;
        let kind = history_kind(next.project_id);
        self.put(&kind, next.version, &next)?;
        for old in self
            .read()
            .checks_history(next.project_id)?
            .into_iter()
            .skip(HISTORY_LIMIT)
        {
            self.delete(&kind, old.version)?;
        }
        // Eager invalidation persists even if a later attempt to apply the
        // stale draft returns an error and its transaction rolls back.
        for mut draft in self.read().checks_drafts(next.project_id)? {
            if open_draft(draft.state) && draft.base_version != next.version {
                draft.state = ChecksDraftState::Stale;
                draft.updated_at = self.now;
                self.put(DRAFTS, draft.id, &draft)?;
            }
        }
        Ok(next)
    }
}

fn require_check(checks: &[Check], id: CheckId) -> Result<&Check> {
    checks
        .iter()
        .find(|check| check.check_id == id)
        .ok_or_else(|| rule(ErrorCode::NotFound, "This check no longer exists."))
}
fn find_check_mut(checks: &mut [Check], id: CheckId) -> Result<&mut Check> {
    checks
        .iter_mut()
        .find(|check| check.check_id == id)
        .ok_or_else(|| rule(ErrorCode::NotFound, "This check no longer exists."))
}
fn upsert_check(checks: &mut Vec<Check>, check: &Check) {
    if let Some(old) = checks.iter_mut().find(|old| old.check_id == check.check_id) {
        *old = check.clone();
    } else {
        checks.push(check.clone());
    }
}
fn apply_item(checks: &mut Vec<Check>, item: &CheckDraftItem) {
    match item {
        CheckDraftItem::Add { check, .. } | CheckDraftItem::Change { check, .. } => {
            upsert_check(checks, check)
        }
        CheckDraftItem::Remove { check_id, .. } => {
            checks.retain(|check| check.check_id != *check_id)
        }
    }
}
fn keep_measurement(check: &mut Check, previous: Option<&Check>) {
    check.typical_secs = previous
        .filter(|old| {
            old.run == check.run
                && old.setup == check.setup
                && old.network == check.network
                && old.timeout_secs == check.timeout_secs
                && old.inputs == check.inputs
        })
        .and_then(|old| old.typical_secs);
}
fn validate_text(text: &str, max: usize, message: &str, multiline: bool) -> Result<()> {
    if text.trim().is_empty()
        || text.len() > max
        || text
            .chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')))
    {
        return Err(invalid(message));
    }
    Ok(())
}
fn validate_checks(checks: &[Check]) -> Result<()> {
    if checks.len() > CHECK_LIMIT {
        return Err(invalid("A project can have at most 64 checks."));
    }
    let mut ids = HashSet::new();
    for check in checks {
        if !ids.insert(check.check_id) {
            return Err(invalid("Each check must have its own identity."));
        }
        validate_check(check)?;
    }
    Ok(())
}
/// Validate the definition itself. File access validation and hashing happen
/// separately, against a capability rooted in the project's own folder.
pub fn validate_check(check: &Check) -> Result<()> {
    validate_text(
        &check.name,
        120,
        "Give the check a plain name of 1 to 120 UTF-8 bytes, without control characters.",
        false,
    )?;
    validate_text(
        &check.about,
        4_000,
        "Describe what the check proves using 1 to 4000 UTF-8 bytes, without unsupported control characters.",
        true,
    )?;
    validate_text(
        &check.run,
        65_536,
        "Give the check a command of 1 to 65536 UTF-8 bytes, without unsupported control characters.",
        true,
    )?;
    if let Some(setup) = &check.setup {
        validate_text(
            setup,
            65_536,
            "Use a setup command of 1 to 65536 UTF-8 bytes, without unsupported control characters.",
            true,
        )?;
    }
    if !(10..=3_600).contains(&check.timeout_secs) {
        return Err(invalid(
            "Choose a time limit between 10 seconds and 60 minutes.",
        ));
    }
    if check
        .typical_secs
        .is_some_and(|secs| !secs.is_finite() || secs < 0.0)
    {
        return Err(invalid("The measured running time is not valid."));
    }
    if check.paths.len() > 64 || check.inputs.len() > 64 {
        return Err(invalid(
            "List at most 64 watched patterns and 64 input files for each check.",
        ));
    }
    for pattern in &check.paths {
        validate_text(
            pattern,
            1_024,
            "Use a watched pattern of 1 to 1024 UTF-8 bytes, without control characters.",
            false,
        )?;
        Glob::new(pattern).map_err(|_| invalid("A watched file pattern could not be understood. Correct the pattern and try again."))?;
    }
    let mut inputs = HashSet::new();
    for input in &check.inputs {
        validate_input_path(input)?;
        if !inputs.insert(input) {
            return Err(invalid("List each input file only once."));
        }
    }
    Ok(())
}
fn validate_input_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4_096
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(rule(
            ErrorCode::PathNotAllowed,
            "A check's input files must be relative paths inside the project folder, at most 4096 UTF-8 bytes each, without control characters.",
        ));
    }
    Ok(())
}

/// Hash through an already opened directory capability. Symlink replacement
/// cannot turn a permitted relative name into access outside the project.
/// Missing inputs are recorded explicitly so later creation is a change too.
pub fn fingerprint_check(project_root: &Path, check: &Check) -> Result<Vec<CheckInputFingerprint>> {
    if check.inputs.is_empty() {
        return Ok(vec![]);
    }
    let dir = Dir::open_ambient_dir(project_root, cap_std::ambient_authority())?;
    check
        .inputs
        .iter()
        .map(|path| {
            validate_input_path(path)?;
            Ok(CheckInputFingerprint {
                check_id: check.check_id,
                path: path.clone(),
                sha256: input_hash(&dir, path)?,
            })
        })
        .collect()
}
fn input_hash(dir: &Dir, path: &str) -> Result<Option<String>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    let mut file = match dir.open_with(path, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(rule(
                ErrorCode::PathNotAllowed,
                "An input file could not be read safely inside this project.",
            ));
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(rule(
            ErrorCode::PathNotAllowed,
            "Each check input must be an ordinary file inside the project.",
        ));
    }
    if metadata.len() > INPUT_BYTE_LIMIT {
        return Err(rule(
            ErrorCode::LimitExceeded,
            "An input file is larger than the 16 MB limit.",
        ));
    }
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 32 * 1024];
    let mut read = 0_u64;
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > INPUT_BYTE_LIMIT {
            return Err(rule(
                ErrorCode::LimitExceeded,
                "An input file is larger than the 16 MB limit.",
            ));
        }
        hash.update(&bytes[..count]);
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}

pub fn changed_inputs(
    project_root: &Path,
    approved: &ProjectChecks,
    check: &Check,
) -> Result<Vec<String>> {
    if check.inputs.is_empty() {
        return Ok(vec![]);
    }
    let dir = Dir::open_ambient_dir(project_root, cap_std::ambient_authority())?;
    let mut changed = Vec::new();
    for path in &check.inputs {
        validate_input_path(path)?;
        // An input replaced by an escaping symlink, a special file or an
        // unreadable file is no longer the approved input. Report the change
        // without reading the replacement or hiding the rest of the Library.
        let matches = input_hash(&dir, path).is_ok_and(|sha256| {
            approved.input_fingerprints.iter().any(|old| {
                old.check_id == check.check_id && old.path == *path && old.sha256 == sha256
            })
        });
        if !matches {
            changed.push(path.clone());
        }
    }
    Ok(changed)
}

fn validate_result(result: &CheckResult) -> Result<()> {
    if !result.duration_secs.is_finite() || result.duration_secs < 0.0 {
        return Err(invalid("The check result has an invalid running time."));
    }
    if result.output.len() > 8_192 {
        return Err(invalid("Check output must be at most 8192 UTF-8 bytes."));
    }
    Ok(())
}

/// These descriptions deliberately use only the approved fields, not the
/// agent's description or change note.
pub fn check_facts(check: &Check) -> CheckFacts {
    let scope = watched_file_scope(&check.paths);
    let when = match (check.when, scope) {
        (CheckWhen::EveryTask, None) => "After every task that changes files".into(),
        (CheckWhen::EveryTask, Some(scope)) => format!("After a task changes {scope}"),
        (CheckWhen::BeforeMerge, None) => "Before merging work; not run automatically yet".into(),
        (CheckWhen::BeforeMerge, Some(scope)) => {
            format!("Before merging work after changes to {scope}; not run automatically yet")
        }
    };
    CheckFacts {
        when,
        internet: if check.network {
            "Uses the internet"
        } else {
            "Does not use the internet"
        }
        .into(),
        time_limit: format!("Time limit: {}", duration_words(check.timeout_secs as f64)),
        typical_time: check
            .typical_secs
            .filter(|secs| secs.is_finite() && *secs >= 0.0)
            .map(|secs| format!("Takes about {}", duration_words(secs))),
    }
}
/// Name folders only when every pattern has a short, literal top-level
/// prefix. Root files and ambiguous glob prefixes must not be called folders.
fn watched_file_scope(patterns: &[String]) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    let fallback = || Some("one of its watched files".to_owned());
    if patterns.len() > 64 {
        return fallback();
    }
    let mut roots = Vec::new();
    for pattern in patterns {
        let Some((root, rest)) = pattern.split_once('/') else {
            return fallback();
        };
        if root.is_empty()
            || root.len() > 40
            || !root
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || rest.is_empty()
            || pattern.contains('\\')
            || pattern
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
            || Glob::new(pattern).is_err()
        {
            return fallback();
        }
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let names = match roots.as_slice() {
        [one] => format!("{one} folder"),
        [one, two] => format!("{one} and {two} folders"),
        [one, two, three] => format!("{one}, {two} and {three} folders"),
        [one, two, three, ..] => format!("{one}, {two}, {three} and other watched folders"),
        [] => return fallback(),
    };
    Some(format!("watched files in the {names}"))
}

/// Factual differences between two definitions. Missing history must be
/// presented as stale by the caller rather than treating notes as facts.
pub fn check_differences(before: &Check, after: &Check) -> Vec<String> {
    let mut changes = Vec::new();
    if before.name != after.name {
        changes.push("Its name changed".into());
    }
    if before.about != after.about {
        changes.push("Its description changed".into());
    }
    if before.run != after.run {
        changes.push("Now runs a different command".into());
    }
    if before.setup != after.setup {
        changes.push("The stored setup command changed; setup is not run yet".into());
    }
    if before.network != after.network {
        changes.push(
            if after.network {
                "Now uses the internet"
            } else {
                "No longer uses the internet"
            }
            .into(),
        );
    }
    if before.timeout_secs != after.timeout_secs {
        changes.push(format!(
            "Time limit changed from {} to {}",
            duration_words(before.timeout_secs as f64),
            duration_words(after.timeout_secs as f64)
        ));
    }
    if before.when != after.when || before.paths != after.paths {
        changes.push(check_facts(after).when);
        if before.paths != after.paths {
            changes.push("The watched file patterns changed".into());
        }
    }
    if before.inputs != after.inputs {
        changes.push("The files it depends on changed".into());
    }
    if before.enabled != after.enabled {
        changes.push(
            if after.enabled {
                "Now turned on"
            } else {
                "Now turned off"
            }
            .into(),
        );
    }
    changes
}
fn duration_words(seconds: f64) -> String {
    let seconds = seconds.round().max(1.0) as u64;
    let (minutes, remainder) = (seconds / 60, seconds % 60);
    match (minutes, remainder) {
        (0, 1) => "1 second".into(),
        (0, seconds) => format!("{seconds} seconds"),
        (1, 0) => "1 minute".into(),
        (minutes, 0) => format!("{minutes} minutes"),
        (minutes, seconds) => format!(
            "{minutes} {} {seconds} {}",
            if minutes == 1 { "minute" } else { "minutes" },
            if seconds == 1 { "second" } else { "seconds" }
        ),
    }
}
