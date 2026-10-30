// SPDX-License-Identifier: Apache-2.0
//! SQLite journal and typed projection documents, committed together.
//!
//! Callers publish `Commit::events` only after `write` returns. A failed closure
//! rolls back both the projection and the journal. Sequence allocation uses
//! AUTOINCREMENT so retention cannot recycle a previously published cursor.
mod approvals;
mod diagnostics;
mod history;
mod instance_lock;
mod native_sessions;
pub mod reports;
mod secret_answers;
pub use diagnostics::{io_error_summary, json_error_summary};
pub use instance_lock::InstanceLock;

use protocol::{
    CommandEnvelope, CommandReceipt, Event, EventEnvelope,
    auth::Principal,
    event::EventSource,
    model::{Provenance, TimelineItem},
    time::Timestamp,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{cell::RefCell, path::Path, time::Duration};

// Decoded projection rows and bytes on this thread, for cost regressions.
#[cfg(feature = "testing")]
thread_local! {
    static READ_COST: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}
#[cfg(feature = "testing")]
pub fn take_read_cost() -> (usize, usize) {
    READ_COST.with(|cost| cost.replace((0, 0)))
}
fn decode<T: DeserializeOwned>(json: &str) -> Result<T> {
    #[cfg(feature = "testing")]
    READ_COST.with(|cost| {
        let (rows, bytes) = cost.get();
        cost.set((rows + 1, bytes + json.len()));
    });
    Ok(serde_json::from_str(json)?)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database operation failed")]
    Sql(#[from] rusqlite::Error),
    #[error("stored value is invalid")]
    Json(#[from] serde_json::Error),
    #[error("database filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("safe wire integer range exhausted")]
    IntegerExhausted,
    #[error(
        "database schema is newer than this version of Øphio; install a compatible newer Øphio, or stop Øphio and restore the pre-migration database backup named <database-stem>.before-v<schema>-<id>.sqlite beside the database before running the previous version; preserve the current database and its -wal/-shm files first"
    )]
    FutureSchema,
    #[error("{0}")]
    Rule(#[from] protocol::ProtocolError),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub max_events: u64,
    pub max_age_ms: i64,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            max_events: 100_000,
            max_age_ms: 30 * 86400 * 1000,
        }
    }
}
#[derive(Debug)]
pub struct Store {
    connection: Connection,
    retention: Retention,
}
#[derive(Debug)]
pub struct Commit<T> {
    pub value: T,
    pub events: Vec<EventEnvelope>,
    pub head: u64,
    kinds: Vec<String>,
}
impl<T> Commit<T> {
    /// Whether this commit put or deleted any document of `kind`.
    pub fn changed(&self, kind: &str) -> bool {
        self.kinds.iter().any(|k| k == kind)
    }
}
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StoredCommand {
    pub envelope: CommandEnvelope,
    pub principal: Principal,
    pub receipt: CommandReceipt,
    pub stage: protocol::command::CommandStage,
    /// Effect not yet durably settled. Recovery never redispatches this work.
    pub pending: bool,
    #[serde(default)]
    pub result: Option<protocol::command::CommandResult>,
    #[serde(default)]
    pub error: Option<protocol::ProtocolError>,
}
/// A bad row is isolated without throwing away either it or its dedupe key.
#[derive(Debug)]
pub enum CommandRead {
    Decoded(Box<StoredCommand>),
    Unreadable { id: String, cause: String },
}

pub fn unknown_command_outcome() -> protocol::ProtocolError {
    protocol::ProtocolError::new(
        protocol::ErrorCode::OutcomeUnknown,
        "Stored command cannot be read; its outcome is unknown. Review its effect before issuing a new command; this command will never be retried",
    )
}

fn decode_command(id: &str, json: &str) -> std::result::Result<StoredCommand, String> {
    let command: StoredCommand = serde_json::from_str(json).map_err(|e| json_error_summary(&e))?;
    if command.envelope.command_id.to_string() != id
        || command.receipt.command_id != command.envelope.command_id
    {
        return Err("stored command id does not match its row or receipt".into());
    }
    Ok(command)
}
#[derive(Debug)]
pub struct JournalRead {
    pub head: u64,
    pub oldest_retained: u64,
    pub events: Vec<EventEnvelope>,
}

impl Store {
    /// Observe executed SQL in downstream regression tests.
    #[cfg(feature = "testing")]
    pub fn trace(&self, callback: Option<fn(rusqlite::trace::TraceEvent<'_>)>) {
        self.connection.trace_v2(
            rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
            callback,
        );
    }

    pub fn open(path: impl AsRef<Path>, retention: Retention) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 1 {
            return Err(Error::FutureSchema);
        }
        if version < 1 {
            // The SQLite backup API includes committed WAL pages. Copying just
            // the database file would omit them after an unclean shutdown.
            let backup_path =
                path.with_extension(format!("before-v1-{}.sqlite", uuid::Uuid::now_v7()));
            let mut backup = Connection::open(backup_path)?;
            rusqlite::backup::Backup::new(&connection, &mut backup)?.run_to_completion(
                128,
                Duration::from_millis(1),
                None,
            )?;
            let tx = connection.transaction()?;
            tx.execute_batch("CREATE TABLE documents (kind TEXT NOT NULL, id TEXT NOT NULL, json TEXT NOT NULL, PRIMARY KEY(kind,id));
                CREATE TABLE commands (id TEXT PRIMARY KEY, json TEXT NOT NULL);
                CREATE TABLE journal (seq INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, json TEXT NOT NULL);
                CREATE INDEX journal_at ON journal(at);
                PRAGMA user_version=1;")?;
            tx.commit()?;
        }
        // Attention items are never deleted. Counting one task's items must not
        // read every item. SQLite maintains the index for any version that
        // opens the database, and older versions simply do not use it.
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS attention_task ON documents(json_extract(json,'$.task_id')) WHERE kind='attention';",
        )?;
        history::install(&mut connection)?;
        approvals::install(&connection)?;
        native_sessions::install(&connection)?;
        secret_answers::scrub(&mut connection)?;
        // Room for every statement the store runs, so none is parsed twice.
        connection.set_prepared_statement_cache_capacity(64);
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self {
            connection,
            retention,
        })
    }
    pub fn read<T>(&mut self, f: impl FnOnce(&Read<'_>) -> Result<T>) -> Result<T> {
        let tx = self.connection.transaction()?;
        let value = f(&Read { connection: &tx })?;
        tx.commit()?;
        Ok(value)
    }
    pub fn write<T>(
        &mut self,
        now: Timestamp,
        f: impl FnOnce(&mut Write<'_>) -> Result<T>,
    ) -> Result<Commit<T>> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut writer = Write {
            tx: &tx,
            now,
            events: Vec::new(),
            kinds: RefCell::default(),
        };
        let value = f(&mut writer)?;
        let events = std::mem::take(&mut writer.events);
        let kinds = writer.kinds.take();
        let head = writer.read().head()?;
        // Delete a prefix only: wall-clock rollback must never make a hole.
        let age_cut: i64 = tx.prepare_cached(AGE_CUT)?.query_row(
            [now.0.saturating_sub(self.retention.max_age_ms.max(0))],
            |r| r.get(0),
        )?;
        let count_cut = head.saturating_sub(self.retention.max_events.max(1));
        let cut = (age_cut as u64).max(count_cut).min(head.saturating_sub(1));
        tx.prepare_cached("DELETE FROM journal WHERE seq <= ?1")?
            .execute([cut as i64])?;
        tx.commit()?;
        Ok(Commit {
            value,
            events,
            head,
            kinds,
        })
    }
}

/// Without the time index SQLite answers `MAX(seq)` by walking the journal
/// backwards from its newest row until one is old enough, which reads every
/// retained event on each commit. The index holds only the few rows aged out
/// since the previous commit, because each commit deletes them.
const AGE_CUT: &str =
    "SELECT COALESCE(MAX(seq),0) FROM journal INDEXED BY journal_at WHERE at < ?1";

/// Without statistics SQLite prefers the primary key, which gives id order for
/// free but reads every attention item to find one task's.
const TASK_ATTENTION: &str = "SELECT json FROM documents INDEXED BY attention_task WHERE kind='attention' AND json_extract(json,'$.task_id')=?1 ORDER BY id";

/// Timeline rows are keyed by their zero-padded sequence, so key order is
/// sequence order and a page reads only its own rows.
const TIMELINE_PAGE: &str =
    "SELECT json FROM documents WHERE kind=?1 AND id < ?2 ORDER BY id DESC LIMIT ?3";
fn timeline_kind(task_id: protocol::TaskId) -> String {
    format!("timeline:{task_id}")
}
fn timeline_key(seq: u64) -> String {
    format!("{seq:020}")
}

#[derive(Debug, Clone, Copy)]
pub struct Read<'a> {
    connection: &'a Connection,
}
impl Read<'_> {
    pub fn get<T: DeserializeOwned>(&self, kind: &str, id: impl ToString) -> Result<Option<T>> {
        self.text(kind, id)?.map(|s| decode(&s)).transpose()
    }
    /// A document's stored JSON, not decoded.
    pub fn text(&self, kind: &str, id: impl ToString) -> Result<Option<String>> {
        Ok(self
            .connection
            .prepare_cached("SELECT json FROM documents WHERE kind=?1 AND id=?2")?
            .query_row(params![kind, id.to_string()], |r| r.get(0))
            .optional()?)
    }
    pub fn require<T: DeserializeOwned>(&self, kind: &str, id: impl ToString) -> Result<T> {
        self.get(kind, id)?.ok_or_else(|| {
            protocol::ProtocolError::new(protocol::ErrorCode::NotFound, "Record not found").into()
        })
    }
    pub fn list<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT json FROM documents WHERE kind=?1 ORDER BY id")?;
        statement
            .query_map([kind], |r| r.get::<_, String>(0))?
            .map(|s| decode(&s?))
            .collect()
    }
    /// `list` with each record's id, for records that do not carry their own.
    pub fn entries<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<(String, T)>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT id,json FROM documents WHERE kind=?1 ORDER BY id")?;
        statement
            .query_map([kind], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (id, json) = row?;
                Ok((id, serde_json::from_str(&json)?))
            })
            .collect()
    }
    /// The attention items that name `task_id`, in id order.
    pub fn task_attention<T: DeserializeOwned>(&self, task_id: protocol::TaskId) -> Result<Vec<T>> {
        let mut statement = self.connection.prepare_cached(TASK_ATTENTION)?;
        statement
            .query_map([task_id.to_string()], |r| r.get::<_, String>(0))?
            .map(|s| decode(&s?))
            .collect()
    }
    /// Up to `limit` of a task's timeline items older than `before_seq`, oldest
    /// first, and whether any older item remains.
    pub fn timeline(
        &self,
        task_id: protocol::TaskId,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Result<(Vec<TimelineItem>, bool)> {
        let mut statement = self.connection.prepare_cached(TIMELINE_PAGE)?;
        let mut items = statement
            .query_map(
                params![
                    timeline_kind(task_id),
                    timeline_key(before_seq.unwrap_or(protocol::MAX_SAFE_INTEGER + 1)),
                    limit.saturating_add(1).min(i64::MAX as usize) as i64
                ],
                |r| r.get::<_, String>(0),
            )?
            .map(|s| decode(&s?))
            .collect::<Result<Vec<TimelineItem>>>()?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        items.reverse();
        Ok((items, has_more))
    }
    pub fn command(&self, id: protocol::CommandId) -> Result<Option<StoredCommand>> {
        if self.command_quarantined(id)? {
            return Err(unknown_command_outcome().into());
        }
        let json: Option<String> = self
            .connection
            .prepare_cached("SELECT json FROM commands WHERE id=?1")?
            .query_row([id.to_string()], |r| r.get(0))
            .optional()?;
        json.map(|s| {
            decode_command(&id.to_string(), &s).map_err(|_| Error::Rule(unknown_command_outcome()))
        })
        .transpose()
    }
    pub fn command_quarantined(&self, id: impl ToString) -> Result<bool> {
        Ok(self
            .connection
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM documents WHERE kind='command_quarantine' AND id=?1)",
            )?
            .query_row([id.to_string()], |r| r.get(0))?)
    }
    /// SQL failures remain fatal; decoding failures belong to individual rows.
    /// Quarantined rows stay reserved even if a later version can decode them.
    pub fn commands(&self) -> Result<Vec<CommandRead>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT id,json FROM commands WHERE NOT EXISTS (SELECT 1 FROM documents WHERE kind='command_quarantine' AND documents.id=commands.id) ORDER BY id")?;
        statement
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .map(|row| {
                let (id, json) = row?;
                Ok(match decode_command(&id, &json) {
                    Ok(command) => CommandRead::Decoded(Box::new(command)),
                    Err(cause) => CommandRead::Unreadable { id, cause },
                })
            })
            .collect()
    }
    pub fn head(&self) -> Result<u64> {
        let head = self
            .connection
            .prepare_cached(
                "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='journal'),0)",
            )?
            .query_row([], |r| r.get::<_, i64>(0))? as u64;
        if head > protocol::MAX_SAFE_INTEGER {
            return Err(Error::IntegerExhausted);
        }
        Ok(head)
    }
    pub fn journal(&self, after: u64) -> Result<JournalRead> {
        self.journal_page(after, usize::MAX)
    }
    /// `journal`, reading at most `limit` events.
    pub fn journal_page(&self, after: u64, limit: usize) -> Result<JournalRead> {
        let head = self.head()?;
        let oldest_retained = self
            .connection
            .prepare_cached("SELECT COALESCE(MIN(seq),?1) FROM journal")?
            .query_row(
                [head.saturating_add(1).min(protocol::MAX_SAFE_INTEGER) as i64],
                |r| r.get::<_, i64>(0),
            )? as u64;
        let mut statement = self
            .connection
            .prepare_cached("SELECT json FROM journal WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
        let events = statement
            .query_map(
                [
                    after.min(i64::MAX as u64) as i64,
                    limit.min(i64::MAX as usize) as i64,
                ],
                |r| r.get::<_, String>(0),
            )?
            .map(|s| decode(&s?))
            .collect::<Result<_>>()?;
        Ok(JournalRead {
            head,
            oldest_retained,
            events,
        })
    }
}

#[derive(Debug)]
pub struct Write<'a> {
    tx: &'a Transaction<'a>,
    now: Timestamp,
    events: Vec<EventEnvelope>,
    /// Kinds of the documents put or deleted so far, for `Commit::changed`.
    kinds: RefCell<Vec<String>>,
}
impl Write<'_> {
    pub fn read(&self) -> Read<'_> {
        Read {
            connection: self.tx,
        }
    }
    /// The events emitted so far in this write.
    pub fn emitted(&self) -> &[EventEnvelope] {
        &self.events
    }
    pub fn put<T: Serialize>(&self, kind: &str, id: impl ToString, value: &T) -> Result<()> {
        self.tx.prepare_cached("INSERT INTO documents(kind,id,json) VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET json=excluded.json")?.execute(params![kind, id.to_string(), safe_json(value)?])?;
        self.changed(kind);
        Ok(())
    }
    pub fn delete(&self, kind: &str, id: impl ToString) -> Result<()> {
        self.tx
            .prepare_cached("DELETE FROM documents WHERE kind=?1 AND id=?2")?
            .execute(params![kind, id.to_string()])?;
        self.changed(kind);
        Ok(())
    }
    /// Runs `f` as one step of this write. When it fails, everything it put,
    /// deleted, recorded or emitted is undone, and the rest of the write
    /// still commits.
    pub fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.tx.execute_batch("SAVEPOINT attempt")?;
        let (events, kinds) = (self.events.len(), self.kinds.borrow().len());
        match f(self) {
            Ok(value) => {
                self.tx.execute_batch("RELEASE attempt")?;
                Ok(value)
            }
            Err(e) => {
                self.tx
                    .execute_batch("ROLLBACK TO attempt; RELEASE attempt")?;
                self.events.truncate(events);
                self.kinds.borrow_mut().truncate(kinds);
                Err(e)
            }
        }
    }
    fn changed(&self, kind: &str) {
        let mut kinds = self.kinds.borrow_mut();
        if !kinds.iter().any(|k| k == kind) {
            kinds.push(kind.to_owned());
        }
    }
    pub fn command(&self, command: &StoredCommand) -> Result<()> {
        if self
            .read()
            .command_quarantined(command.envelope.command_id)?
        {
            return Err(unknown_command_outcome().into());
        }
        self.tx.prepare_cached("INSERT INTO commands(id,json) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET json=excluded.json")?.execute(params![command.envelope.command_id.to_string(), safe_json(command)?])?;
        Ok(())
    }
    /// Keep the original bytes for diagnosis and reserve the id permanently.
    /// The marker and progress event commit together with startup recovery.
    pub fn quarantine_command(&mut self, id: &str) -> Result<()> {
        if self.read().command_quarantined(id)? {
            return Ok(());
        }
        let error = unknown_command_outcome();
        let stage = protocol::command::CommandStage::ReconciliationRequired;
        self.put(
            "command_quarantine",
            id,
            &serde_json::json!({
                "stage": stage, "pending": false, "error": error,
            }),
        )?;
        // An invalid row key cannot be represented on the wire. Its durable
        // marker still reserves it; never invent a different command identity.
        if let Ok(command_id) = id.parse() {
            self.emit(Event::CommandProgress {
                command_id,
                stage,
                result: None,
                error: Some(error),
                detail: Some(
                    "Stored command is unreadable; outcome unknown; review required".into(),
                ),
            })?;
        }
        Ok(())
    }
    pub fn emit(&mut self, event: Event) -> Result<u64> {
        self.emit_from(event, EventSource::Companion, Provenance::Observed)
    }
    pub fn emit_from(
        &mut self,
        mut event: Event,
        source: EventSource,
        provenance: Provenance,
    ) -> Result<u64> {
        if self.read().head()? >= protocol::MAX_SAFE_INTEGER {
            return Err(Error::IntegerExhausted);
        }
        self.tx
            .prepare_cached("INSERT INTO journal(at,json) VALUES(?1,'')")?
            .execute([self.now.0])?;
        let seq = self.tx.last_insert_rowid() as u64;
        if let Event::TimelineAppended { item, .. } = &mut event {
            item.seq = seq;
        }
        let envelope = EventEnvelope {
            seq,
            at: self.now,
            source,
            provenance,
            event,
        };
        self.tx
            .prepare_cached("UPDATE journal SET json=?1 WHERE seq=?2")?
            .execute(params![safe_json(&envelope)?, seq as i64])?;
        if let Event::TimelineAppended { task_id, item } = &envelope.event {
            self.put(&timeline_kind(*task_id), timeline_key(seq), item)?;
        }
        self.events.push(envelope);
        Ok(seq)
    }
}

/// Compact JSON of `value`, refusing every number `protocol::integer::validate`
/// refuses. Checking while writing spares building a `serde_json::Value` of
/// each record first.
fn safe_json<T: Serialize>(value: &T) -> Result<String> {
    let mut json = Vec::with_capacity(256);
    let mut serializer = serde_json::Serializer::with_formatter(&mut json, SafeNumbers::default());
    value.serialize(&mut serializer).map_err(|e| {
        if e.is_io() {
            Error::IntegerExhausted
        } else {
            Error::Json(e)
        }
    })?;
    String::from_utf8(json).map_err(|_| Error::Json(serde::ser::Error::custom("invalid UTF-8")))
}

/// `serde_json`'s compact formatter, failing on a number outside the safe wire
/// range. Numbers inside strings are map keys, which validation sees as text.
#[derive(Default)]
struct SafeNumbers {
    in_string: bool,
}
impl SafeNumbers {
    fn check(&self, unsafe_number: bool) -> std::io::Result<()> {
        if unsafe_number && !self.in_string {
            return Err(std::io::Error::other("number exceeds the safe wire range"));
        }
        Ok(())
    }
}
impl serde_json::ser::Formatter for SafeNumbers {
    fn write_i64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: i64) -> std::io::Result<()> {
        self.check(v.unsigned_abs() > protocol::MAX_SAFE_INTEGER)?;
        serde_json::ser::CompactFormatter.write_i64(w, v)
    }
    fn write_i128<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        v: i128,
    ) -> std::io::Result<()> {
        self.check(v.unsigned_abs() > u128::from(protocol::MAX_SAFE_INTEGER))?;
        serde_json::ser::CompactFormatter.write_i128(w, v)
    }
    fn write_u64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: u64) -> std::io::Result<()> {
        self.check(v > protocol::MAX_SAFE_INTEGER)?;
        serde_json::ser::CompactFormatter.write_u64(w, v)
    }
    fn write_u128<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        v: u128,
    ) -> std::io::Result<()> {
        self.check(v > u128::from(protocol::MAX_SAFE_INTEGER))?;
        serde_json::ser::CompactFormatter.write_u128(w, v)
    }
    /// As `serde_json::Value` held it: widened, so the text is unchanged.
    fn write_f32<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: f32) -> std::io::Result<()> {
        self.write_f64(w, v.into())
    }
    fn write_f64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: f64) -> std::io::Result<()> {
        self.check(v.fract() == 0.0 && v.abs() > protocol::MAX_SAFE_INTEGER as f64)?;
        serde_json::ser::CompactFormatter.write_f64(w, v)
    }
    fn begin_string<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.in_string = true;
        serde_json::ser::CompactFormatter.begin_string(w)
    }
    fn end_string<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.in_string = false;
        serde_json::ser::CompactFormatter.end_string(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path() -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("store-test-{}", uuid::Uuid::now_v7()))
            .join("db")
    }
    #[test]
    fn safe_sequence_and_revision_limits_roll_back_atomically() {
        let path = path();
        let mut db = Store::open(&path, Retention::default()).unwrap();
        db.connection
            .execute(
                "INSERT INTO sqlite_sequence(name,seq) VALUES('journal',?1)",
                [protocol::MAX_SAFE_INTEGER as i64 - 1],
            )
            .unwrap();
        let event = Event::BootEpochChanged {
            boot_epoch: protocol::BootEpoch::new(),
        };
        let commit = db.write(Timestamp(0), |tx| tx.emit(event.clone())).unwrap();
        assert_eq!(commit.head, protocol::MAX_SAFE_INTEGER);
        let failure = db
            .write(Timestamp(0), |tx| {
                tx.put("test", "rollback", &true)?;
                tx.emit(event)
            })
            .unwrap_err();
        assert!(matches!(failure, Error::IntegerExhausted));
        db.read(|r| {
            assert!(r.get::<bool>("test", "rollback")?.is_none());
            assert_eq!(r.journal(protocol::MAX_SAFE_INTEGER - 1)?.events.len(), 1);
            Ok(())
        })
        .unwrap();
        let mut queue = protocol::queue::QueueView {
            project_id: Default::default(),
            revision: protocol::MAX_SAFE_INTEGER,
            paused: true,
            pause_reason: Some("Next up starts paused. Resume it to let tasks start.".into()),
            writer: None,
            items: vec![],
        };
        db.write(Timestamp(0), |tx| tx.put("queue", queue.project_id, &queue))
            .unwrap();
        queue.revision += 1;
        assert!(matches!(
            db.write(Timestamp(0), |tx| tx.put("queue", queue.project_id, &queue)),
            Err(Error::IntegerExhausted)
        ));
        assert_eq!(
            db.read(|r| Ok(r
                .require::<protocol::queue::QueueView>("queue", queue.project_id)?
                .revision))
                .unwrap(),
            protocol::MAX_SAFE_INTEGER
        );
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn a_failed_attempt_is_undone_and_the_rest_of_the_write_commits() {
        let path = path();
        let mut db = Store::open(&path, Retention::default()).unwrap();
        let event = || Event::BootEpochChanged {
            boot_epoch: protocol::BootEpoch::new(),
        };
        let commit = db
            .write(Timestamp(0), |tx| {
                tx.put("test", "before", &true)?;
                tx.emit(event())?;
                let failed = tx.attempt(|tx| {
                    tx.put("test", "before", &false)?;
                    tx.put("undone", "inside", &true)?;
                    tx.emit(event())?;
                    Err::<(), _>(Error::IntegerExhausted)
                });
                assert!(matches!(failed, Err(Error::IntegerExhausted)));
                tx.attempt(|tx| {
                    tx.put("test", "kept", &true)?;
                    tx.emit(event())
                })
            })
            .unwrap();
        assert_eq!(commit.events.len(), 2);
        assert_eq!(commit.value, commit.head);
        assert!(commit.changed("test") && !commit.changed("undone"));
        db.read(|r| {
            assert_eq!(r.get::<bool>("test", "before")?, Some(true));
            assert_eq!(r.get::<bool>("test", "kept")?, Some(true));
            assert!(r.get::<bool>("undone", "inside")?.is_none());
            assert_eq!(r.journal(0)?.events.len(), 2);
            Ok(())
        })
        .unwrap();
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn stored_json_refuses_exactly_what_integer_validation_refuses() {
        /// The earlier `to_value`, `validate`, `to_string` outcome, decoded.
        fn same_as_before<T: Serialize>(case: T) {
            let value = serde_json::to_value(&case).unwrap();
            let accepted = protocol::integer::validate(&value).is_ok();
            let before: serde_json::Value =
                serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
            match safe_json(&case) {
                Ok(json) => {
                    assert!(accepted, "{value} was refused before");
                    let stored: serde_json::Value = serde_json::from_str(&json).unwrap();
                    assert_eq!(stored, before, "{json}");
                }
                Err(Error::IntegerExhausted) => assert!(!accepted, "{value} was accepted before"),
                Err(e) => panic!("{value}: {e:?}"),
            }
        }
        let max = protocol::MAX_SAFE_INTEGER;
        same_as_before(max);
        same_as_before(max + 1);
        same_as_before(u64::MAX);
        same_as_before(-(max as i64));
        same_as_before(-(max as i64) - 1);
        same_as_before(i64::MIN);
        same_as_before(u128::from(max));
        same_as_before(u128::from(max) + 1);
        same_as_before(-i128::from(max) - 1);
        same_as_before(0.5f64);
        same_as_before(max as f64);
        same_as_before((max + 1) as f64);
        same_as_before(-1e300f64);
        same_as_before(f64::NAN);
        same_as_before(1.1f32);
        same_as_before(3e16f32);
        same_as_before(vec![Some(max), None, Some(max + 1)]);
        // Map keys are text to validation, whatever their size.
        same_as_before(std::collections::BTreeMap::from([
            (max + 1, "key"),
            (u64::MAX, "key"),
        ]));
        same_as_before(serde_json::json!({"a": [1, {"b": max + 1}], "c": "123456789012345678901"}));
        same_as_before(serde_json::json!({"a": [1, {"b": max}], "c": null, "d": -2.5}));
        same_as_before(Event::BootEpochChanged {
            boot_epoch: protocol::BootEpoch::new(),
        });
    }
    #[test]
    fn a_commit_names_the_kinds_it_changed() {
        let path = path();
        let mut db = Store::open(&path, Retention::default()).unwrap();
        let commit = db
            .write(Timestamp(0), |tx| {
                tx.put("device", "phone", &1)?;
                tx.put("device", "tablet", &2)?;
                tx.delete("channel", "chat")?;
                let _: Option<u8> = tx.read().get("task", "read only")?;
                tx.emit(Event::BootEpochChanged {
                    boot_epoch: protocol::BootEpoch::new(),
                })
            })
            .unwrap();
        assert!(commit.changed("device") && commit.changed("channel"));
        assert!(!commit.changed("task") && !commit.changed("dev"));
        let commit = db
            .write(Timestamp(0), |tx| tx.read().get::<u8>("device", "phone"))
            .unwrap();
        assert!(!commit.changed("device"));
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn retention_reads_only_aged_events() {
        let path = path();
        let db = Store::open(&path, Retention::default()).unwrap();
        let plan: String = db
            .connection
            .query_row(&format!("EXPLAIN QUERY PLAN {AGE_CUT}"), [0], |r| r.get(3))
            .unwrap();
        assert!(plan.contains("INDEX journal_at (at<?)"), "{plan}");
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn task_attention_reads_only_that_task_through_its_index() {
        let path = path();
        let mut db = Store::open(&path, Retention::default()).unwrap();
        let (task, other) = (protocol::TaskId::new(), protocol::TaskId::new());
        db.write(Timestamp(0), |tx| {
            tx.put(
                "attention",
                "a2",
                &serde_json::json!({"task_id": task, "n": 2}),
            )?;
            tx.put(
                "attention",
                "a1",
                &serde_json::json!({"task_id": task, "n": 1}),
            )?;
            tx.put(
                "attention",
                "b",
                &serde_json::json!({"task_id": other, "n": 3}),
            )?;
            tx.put(
                "attention",
                "c",
                &serde_json::json!({"task_id": null, "n": 4}),
            )?;
            tx.put("other", "d", &serde_json::json!({"task_id": task, "n": 5}))
        })
        .unwrap();
        // A database created before the index gains it when next opened.
        db.connection
            .execute("DROP INDEX attention_task", [])
            .unwrap();
        drop(db);
        let mut db = Store::open(&path, Retention::default()).unwrap();
        let found = db
            .read(|r| r.task_attention::<serde_json::Value>(task))
            .unwrap();
        assert_eq!(
            found
                .iter()
                .map(|v| v["n"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        let plan: String = db
            .connection
            .query_row(&format!("EXPLAIN QUERY PLAN {TASK_ATTENTION}"), [""], |r| {
                r.get(3)
            })
            .unwrap();
        assert!(plan.contains("USING INDEX attention_task"), "{plan}");
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn timeline_pages_match_the_whole_history_and_read_one_page() {
        let path = path();
        let mut db = Store::open(&path, Retention::default()).unwrap();
        let (task, other) = (protocol::TaskId::new(), protocol::TaskId::new());
        let note = |task_id, text: &str| Event::TimelineAppended {
            task_id,
            item: TimelineItem {
                seq: 0,
                at: Timestamp(0),
                run_id: None,
                item: protocol::model::TimelineBody::Note { text: text.into() },
            },
        };
        db.write(Timestamp(0), |tx| {
            for i in 0..12 {
                tx.emit(note(task, &format!("mine {i}")))?;
                if i % 3 == 0 {
                    tx.emit(note(other, "other"))?;
                    tx.emit(Event::BootEpochChanged {
                        boot_epoch: protocol::BootEpoch::new(),
                    })?;
                }
            }
            Ok(())
        })
        .unwrap();
        db.read(|r| {
            let all: Vec<TimelineItem> = r.list(&format!("timeline:{task}"))?;
            assert_eq!(all.len(), 12);
            let head = r.head()?;
            for before in (0..=head + 2).map(Some).chain([None]) {
                for limit in [1, 2, 5, 12, 13, 1000] {
                    let mut expected = all.clone();
                    expected.retain(|i| before.is_none_or(|b| i.seq < b));
                    let has_more = expected.len() > limit;
                    if has_more {
                        expected.drain(..expected.len() - limit);
                    }
                    assert_eq!(r.timeline(task, before, limit)?, (expected, has_more));
                }
            }
            Ok(())
        })
        .unwrap();
        let plan: String = db
            .connection
            .query_row(
                &format!("EXPLAIN QUERY PLAN {TIMELINE_PAGE}"),
                params!["", "", 1],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("(kind=? AND id<?)"), "{plan}");
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn rollback_retention_and_restart() {
        let path = path();
        let mut db = Store::open(
            &path,
            Retention {
                max_events: 3,
                max_age_ms: 1000,
            },
        )
        .unwrap();
        let failed: Result<Commit<()>> = db.write(Timestamp(1), |tx| {
            tx.put("test", "a", &42)?;
            tx.emit(Event::BootEpochChanged {
                boot_epoch: protocol::BootEpoch::new(),
            })?;
            Err(protocol::ProtocolError::new(protocol::ErrorCode::Conflict, "rollback").into())
        });
        assert!(failed.is_err());
        db.read(|r| {
            assert!(r.get::<u32>("test", "a")?.is_none());
            assert_eq!(r.head()?, 0);
            Ok(())
        })
        .unwrap();
        for i in 0..10 {
            db.write(Timestamp(i), |tx| {
                tx.emit(Event::BootEpochChanged {
                    boot_epoch: protocol::BootEpoch::new(),
                })
            })
            .unwrap();
        }
        drop(db);
        let mut db = Store::open(&path, Retention::default()).unwrap();
        db.read(|r| {
            let j = r.journal(0)?;
            assert_eq!(j.head, 10);
            assert_eq!(j.oldest_retained, 8);
            assert_eq!(
                j.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
                vec![8, 9, 10]
            );
            Ok(())
        })
        .unwrap();
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

#[cfg(test)]
mod migrations;

mod project_stage;
