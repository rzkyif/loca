//! SQLite-backed CRUD over `scheduled_prompts`, owner-scoped. Every write path
//! (POST, PATCH, the agent schedule-confirm flow) passes through here, which is
//! why validation lives here rather than in a route handler.

use rusqlite::{Connection, OptionalExtension, Row as SqlRow};
use serde::{Deserialize, Serialize};

use crate::contract::api::{ScheduleSpec, ScheduledPromptDraft};
use crate::db::Db;
use crate::error::{not_found, validation, Result};

use super::spec::{next_occurrence, validate_spec};

/// Per-owner bound; only a runaway tool proposing schedules in a loop hits it.
const MAX_SCHEDULES_PER_OWNER: i64 = 100;

const COLUMNS: &str = "id, owner_client_id, title, instruction, schedule_json, \
                       run_missed, enabled, last_run_at_ms, next_run_at_ms, \
                       created_at_ms, updated_at_ms";

/// A stored schedule as the client reads it. Hand-written because the contract
/// bridge only sees the draft. The two timestamps are omitted when absent, not
/// `null`: an absent `nextRunAtMs` is how the client tells dormant from armed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduledPrompt {
    pub id: String,
    #[serde(rename = "ownerClientId")]
    pub owner_client_id: String,
    pub title: String,
    pub instruction: String,
    pub schedule: ScheduleSpec,
    #[serde(rename = "runMissed")]
    pub run_missed: bool,
    pub enabled: bool,
    #[serde(rename = "lastRunAtMs", skip_serializing_if = "Option::is_none")]
    pub last_run_at_ms: Option<i64>,
    #[serde(rename = "nextRunAtMs", skip_serializing_if = "Option::is_none")]
    pub next_run_at_ms: Option<i64>,
    #[serde(rename = "createdAtMs")]
    pub created_at_ms: i64,
    #[serde(rename = "updatedAtMs")]
    pub updated_at_ms: i64,
}

/// The row as stored, with `schedule_json` still a string so the tick can park
/// a row whose JSON will not parse.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub owner_client_id: String,
    pub title: String,
    pub instruction: String,
    pub schedule_json: String,
    pub run_missed: bool,
    pub enabled: bool,
    pub last_run_at_ms: Option<i64>,
    pub next_run_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl Row {
    fn from_sql(row: &SqlRow<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            owner_client_id: row.get(1)?,
            title: row.get(2)?,
            instruction: row.get(3)?,
            schedule_json: row.get(4)?,
            run_missed: row.get::<_, i64>(5)? == 1,
            enabled: row.get::<_, i64>(6)? == 1,
            last_run_at_ms: row.get(7)?,
            next_run_at_ms: row.get(8)?,
            created_at_ms: row.get(9)?,
            updated_at_ms: row.get(10)?,
        })
    }

    /// The stored schedule, or an error naming the row.
    pub fn schedule(&self) -> Result<ScheduleSpec> {
        serde_json::from_str(&self.schedule_json).map_err(|err| {
            validation(format!(
                "scheduled prompt {} has an unreadable schedule: {err}",
                self.id
            ))
        })
    }

    pub fn into_prompt(self) -> Result<ScheduledPrompt> {
        let schedule = self.schedule()?;
        Ok(ScheduledPrompt {
            id: self.id,
            owner_client_id: self.owner_client_id,
            title: self.title,
            instruction: self.instruction,
            schedule,
            run_missed: self.run_missed,
            enabled: self.enabled,
            last_run_at_ms: self.last_run_at_ms,
            next_run_at_ms: self.next_run_at_ms,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        })
    }
}

/// The editable fields of a `PATCH`. Every one optional: a patch carrying
/// nothing is a no-op that still returns the prompt.
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub title: Option<String>,
    pub instruction: Option<String>,
    pub schedule: Option<ScheduleSpec>,
    pub run_missed: Option<bool>,
    pub enabled: Option<bool>,
}

/// A validated, fully-derived row on its way into the table. `prepare` needs no
/// connection and `insert` does, so the async and blocking creates share both.
struct NewRow {
    id: String,
    owner_client_id: String,
    title: String,
    instruction: String,
    schedule_json: String,
    run_missed: i64,
    next_run_at_ms: i64,
    now: i64,
}

impl NewRow {
    fn prepare(owner_client_id: &str, draft: &ScheduledPromptDraft) -> Result<Self> {
        validate_spec(&draft.schedule)?;
        let now = now_ms();
        // Only a passed `once` has no occurrence; saving it could never fire.
        let next_run_at_ms = next_occurrence(&draft.schedule, now)
            .ok_or_else(|| validation("the scheduled time is already in the past"))?;
        let schedule_json = serde_json::to_string(&draft.schedule).map_err(|err| {
            crate::error::internal(format!("could not serialise the schedule: {err}"))
        })?;
        Ok(Self {
            id: ulid::Ulid::new().to_string(),
            owner_client_id: owner_client_id.to_string(),
            title: draft.title.clone(),
            instruction: draft.instruction.clone(),
            schedule_json,
            run_missed: i64::from(draft.run_missed),
            next_run_at_ms,
            now,
        })
    }

    fn insert(&self, conn: &mut Connection) -> Result<()> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM scheduled_prompts WHERE owner_client_id = ?",
            [&self.owner_client_id],
            |row| row.get(0),
        )?;
        if count >= MAX_SCHEDULES_PER_OWNER {
            return Err(validation(format!(
                "scheduled prompt limit reached ({MAX_SCHEDULES_PER_OWNER}); delete one first"
            )));
        }
        conn.execute(
            "INSERT INTO scheduled_prompts
               (id, owner_client_id, title, instruction, schedule_json, run_missed,
                enabled, next_run_at_ms, created_at_ms, updated_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
            rusqlite::params![
                self.id,
                self.owner_client_id,
                self.title,
                self.instruction,
                self.schedule_json,
                self.run_missed,
                self.next_run_at_ms,
                self.now,
                self.now
            ],
        )?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct Store {
    db: Db,
}

impl Store {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // --- owner-scoped CRUD ---------------------------------------------------

    pub async fn list(&self, owner_client_id: &str) -> Result<Vec<ScheduledPrompt>> {
        let owner = owner_client_id.to_string();
        let rows = self
            .db
            .call(move |conn| {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM scheduled_prompts \
                     WHERE owner_client_id = ? ORDER BY created_at_ms ASC"
                ))?;
                let rows = stmt
                    .query_map([owner], Row::from_sql)?
                    .collect::<rusqlite::Result<Vec<Row>>>()?;
                Ok(rows)
            })
            .await?;
        // Drop an unparseable row rather than fail the list; the tick parks it.
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let id = row.id.clone();
                row.into_prompt()
                    .inspect_err(|err| {
                        tracing::error!(
                            target: "loca::scheduled",
                            "omitting scheduled prompt {id} from the list: {err}",
                        );
                    })
                    .ok()
            })
            .collect())
    }

    pub async fn get(&self, owner_client_id: &str, id: &str) -> Result<ScheduledPrompt> {
        self.row(owner_client_id, id).await?.into_prompt()
    }

    async fn row(&self, owner_client_id: &str, id: &str) -> Result<Row> {
        let owner = owner_client_id.to_string();
        let wanted = id.to_string();
        let missing = format!("scheduled prompt {id} not found");
        self.db
            .call(move |conn| {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM scheduled_prompts WHERE id = ? AND owner_client_id = ?"
                ))?;
                let row = stmt
                    .query_row([&wanted, &owner], Row::from_sql)
                    .optional()?;
                row.ok_or_else(|| not_found(missing))
            })
            .await
    }

    pub async fn create(
        &self,
        owner_client_id: &str,
        draft: &ScheduledPromptDraft,
    ) -> Result<ScheduledPrompt> {
        let new = NewRow::prepare(owner_client_id, draft)?;
        let id = new.id.clone();
        self.db.call(move |conn| new.insert(conn)).await?;
        self.get(owner_client_id, &id).await
    }

    /// [`Self::create`] for the synchronous schedule-confirm path.
    pub fn create_blocking(
        &self,
        owner_client_id: &str,
        draft: &ScheduledPromptDraft,
    ) -> Result<()> {
        let new = NewRow::prepare(owner_client_id, draft)?;
        self.db.call_blocking(|conn| new.insert(conn))
    }

    pub async fn update(
        &self,
        owner_client_id: &str,
        id: &str,
        patch: &Patch,
    ) -> Result<ScheduledPrompt> {
        let current = self.get(owner_client_id, id).await?;
        if let Some(schedule) = &patch.schedule {
            validate_spec(schedule)?;
        }
        let schedule = patch.schedule.clone().unwrap_or(current.schedule);
        let enabled = patch.enabled.unwrap_or(current.enabled);

        // Keep the armed occurrence unless something that defines it changed;
        // recomputing on a rename would silently push the schedule forward.
        let next = if !enabled {
            None
        } else if patch.schedule.is_some() || !current.enabled {
            Some(
                next_occurrence(&schedule, now_ms())
                    .ok_or_else(|| validation("the scheduled time is already in the past"))?,
            )
        } else {
            current.next_run_at_ms
        };

        let schedule_json = serde_json::to_string(&schedule).map_err(|err| {
            crate::error::internal(format!("could not serialise the schedule: {err}"))
        })?;
        let title = patch.title.clone().unwrap_or(current.title);
        let instruction = patch.instruction.clone().unwrap_or(current.instruction);
        let run_missed = i64::from(patch.run_missed.unwrap_or(current.run_missed));
        let target = id.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE scheduled_prompts
                        SET title = ?, instruction = ?, schedule_json = ?, run_missed = ?,
                            enabled = ?, next_run_at_ms = ?, updated_at_ms = ?
                      WHERE id = ?",
                    rusqlite::params![
                        title,
                        instruction,
                        schedule_json,
                        run_missed,
                        i64::from(enabled),
                        next,
                        now_ms(),
                        target
                    ],
                )?;
                Ok(())
            })
            .await?;
        self.get(owner_client_id, id).await
    }

    pub async fn delete(&self, owner_client_id: &str, id: &str) -> Result<()> {
        // Read first so a foreign or unknown id is `not_found`.
        self.row(owner_client_id, id).await?;
        let target = id.to_string();
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM scheduled_prompts WHERE id = ?", [target])?;
                Ok(())
            })
            .await
    }

    // --- the tick's reads and writes -----------------------------------------

    /// Every enabled row whose occurrence has arrived.
    pub async fn due(&self, now: i64) -> Result<Vec<Row>> {
        self.db
            .call(move |conn| {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM scheduled_prompts \
                     WHERE enabled = 1 AND next_run_at_ms IS NOT NULL AND next_run_at_ms <= ?"
                ))?;
                let rows = stmt
                    .query_map([now], Row::from_sql)?
                    .collect::<rusqlite::Result<Vec<Row>>>()?;
                Ok(rows)
            })
            .await
    }

    /// The soonest armed occurrence across every owner, which is what the timer
    /// points at.
    pub async fn soonest(&self) -> Result<Option<i64>> {
        self.db
            .call(|conn| {
                let soonest: Option<i64> = conn.query_row(
                    "SELECT MIN(next_run_at_ms) FROM scheduled_prompts \
                     WHERE enabled = 1 AND next_run_at_ms IS NOT NULL",
                    [],
                    |row| row.get(0),
                )?;
                Ok(soonest)
            })
            .await
    }

    /// Re-read one row by id, ignoring ownership (the tick's post-await re-read).
    pub async fn by_id(&self, id: &str) -> Result<Option<Row>> {
        let wanted = id.to_string();
        self.db
            .call(move |conn| {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM scheduled_prompts WHERE id = ?"
                ))?;
                Ok(stmt.query_row([wanted], Row::from_sql).optional()?)
            })
            .await
    }

    /// Record a run and move the armed occurrence on. Called BEFORE the turn
    /// starts, which is what makes firing at-most-once.
    pub async fn advance(&self, id: &str, last_run_at_ms: i64, next: Option<i64>) -> Result<()> {
        let target = id.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE scheduled_prompts SET last_run_at_ms = ?, next_run_at_ms = ? \
                     WHERE id = ?",
                    rusqlite::params![last_run_at_ms, next, target],
                )?;
                Ok(())
            })
            .await
    }

    /// Move the armed occurrence on WITHOUT recording a run. The missed-and-not
    /// -making-it-up path: the occurrence is consumed, but nothing ran.
    pub async fn reschedule(&self, id: &str, next: Option<i64>) -> Result<()> {
        let target = id.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE scheduled_prompts SET next_run_at_ms = ? WHERE id = ?",
                    rusqlite::params![next, target],
                )?;
                Ok(())
            })
            .await
    }

    /// Clear the armed occurrence, leaving the row enabled but dormant (what a
    /// corrupt `schedule_json` gets; a user edit revives it).
    pub async fn park(&self, id: &str) -> Result<()> {
        self.reschedule(id, None).await
    }

    /// Stamp a manual run. Leaves the armed occurrence alone: "Run Now" is
    /// extra, not instead of.
    pub async fn stamp_run(&self, id: &str, at_ms: i64) -> Result<()> {
        let target = id.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE scheduled_prompts SET last_run_at_ms = ? WHERE id = ?",
                    rusqlite::params![at_ms, target],
                )?;
                Ok(())
            })
            .await
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
