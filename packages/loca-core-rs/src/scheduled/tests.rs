//! The scheduled-prompts suite.
//!
//! Split the way the module is: the calendar is pure and tested against measured
//! values, the store against a real SQLite, the tick driven by an explicit `now`
//! so nothing sleeps, and the timer with bounded waits only. An unbounded drain
//! would HANG the whole `--lib` run rather than fail it, so there is not one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::spec::{from_inbound, next_occurrence_in, validate_spec};
use super::testzone::TestZone;
use crate::contract::api::ScheduleSpec;

// --- helpers -----------------------------------------------------------------

/// The instant of a wall clock in a zone.
fn at(tz: &TestZone, y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
    use chrono::TimeZone as _;
    tz.with_ymd_and_hms(y, m, d, h, min, 0)
        .earliest()
        .expect("the fixture never asks for a skipped wall clock")
        .timestamp_millis()
}

fn weekly(weekdays: &[i64], hour: i64, minute: i64) -> ScheduleSpec {
    ScheduleSpec::Weekly {
        weekdays: weekdays.to_vec(),
        hour,
        minute,
    }
}

// --- the calendar ------------------------------------------------------------

#[test]
fn a_once_fires_only_while_it_is_still_in_the_future() {
    let tz = TestZone::fixed(0.0);
    let spec = ScheduleSpec::Once { at_ms: 5_000 };
    assert_eq!(next_occurrence_in(&tz, &spec, 4_999), Some(5_000));
    // Strictly after: the instant itself is not "after" itself, which is what
    // stops a fired schedule re-arming onto the occurrence it just ran.
    assert_eq!(next_occurrence_in(&tz, &spec, 5_000), None);
    assert_eq!(next_occurrence_in(&tz, &spec, 5_001), None);
}

#[test]
fn an_interval_counts_from_the_given_instant_not_from_its_last_run() {
    let tz = TestZone::fixed(0.0);
    let spec = ScheduleSpec::Interval { every_minutes: 90 };
    assert_eq!(
        next_occurrence_in(&tz, &spec, 1_000_000),
        Some(1_000_000 + 90 * 60_000)
    );
}

#[test]
fn an_interval_that_would_overflow_answers_none_rather_than_wrapping() {
    let tz = TestZone::fixed(0.0);
    let spec = ScheduleSpec::Interval {
        every_minutes: i64::MAX,
    };
    assert_eq!(next_occurrence_in(&tz, &spec, 0), None);
}

#[test]
fn a_weekly_picks_the_soonest_listed_weekday_and_wraps_the_week() {
    let tz = TestZone::new_york_2026();
    // 2026-06-03 is a Wednesday (weekday 3).
    let wednesday_noon = at(&tz, 2026, 6, 3, 12, 0);
    // Monday (1) and Friday (5): Friday is sooner from Wednesday.
    assert_eq!(
        next_occurrence_in(&tz, &weekly(&[1, 5], 9, 0), wednesday_noon),
        Some(at(&tz, 2026, 6, 5, 9, 0)),
    );
    // Monday alone wraps into the following week.
    assert_eq!(
        next_occurrence_in(&tz, &weekly(&[1], 9, 0), wednesday_noon),
        Some(at(&tz, 2026, 6, 8, 9, 0)),
    );
    // Today, but the wall clock has passed: the eighth candidate is what makes
    // this land next Wednesday rather than answering None.
    assert_eq!(
        next_occurrence_in(&tz, &weekly(&[3], 9, 0), wednesday_noon),
        Some(at(&tz, 2026, 6, 10, 9, 0)),
    );
    // Today, and the wall clock is still ahead.
    assert_eq!(
        next_occurrence_in(&tz, &weekly(&[3], 18, 30), wednesday_noon),
        Some(at(&tz, 2026, 6, 3, 18, 30)),
    );
}

#[test]
fn a_monthly_clamps_the_day_to_the_length_of_the_month() {
    let tz = TestZone::new_york_2026();
    let spec = ScheduleSpec::Monthly {
        day: 31,
        hour: 9,
        minute: 0,
    };
    // From mid-February 2026, the 31st clamps to the 28th.
    assert_eq!(
        next_occurrence_in(&tz, &spec, at(&tz, 2026, 2, 10, 12, 0)),
        Some(at(&tz, 2026, 2, 28, 9, 0)),
    );
    // From the end of February it rolls into March, which has a real 31st.
    assert_eq!(
        next_occurrence_in(&tz, &spec, at(&tz, 2026, 2, 28, 12, 0)),
        Some(at(&tz, 2026, 3, 31, 9, 0)),
    );
    // December rolls the YEAR, which this has to do explicitly.
    assert_eq!(
        next_occurrence_in(&tz, &spec, at(&tz, 2026, 12, 31, 12, 0)),
        Some(at(&tz, 2027, 1, 31, 9, 0)),
    );
}

#[test]
fn a_yearly_wraps_the_year_and_clamps_the_29th_of_february() {
    let tz = TestZone::new_york_2026();
    let leap_day = ScheduleSpec::Yearly {
        month: 2,
        day: 29,
        hour: 9,
        minute: 0,
    };
    // 2027 is not a leap year, so it clamps to the 28th.
    assert_eq!(
        next_occurrence_in(&tz, &leap_day, at(&tz, 2026, 6, 1, 12, 0)),
        Some(at(&tz, 2027, 2, 28, 9, 0)),
    );
    // From after that, the next candidate is 2028, which IS a leap year.
    assert_eq!(
        next_occurrence_in(&tz, &leap_day, at(&tz, 2027, 6, 1, 12, 0)),
        Some(at(&tz, 2028, 2, 29, 9, 0)),
    );
}

// --- DST, against measured values --------------------------------------------
//
// Both zones agree, in both hemispheres: the offset in force BEFORE the
// transition is the one used, for a skipped local time and a repeated one alike.

#[test]
fn a_weekly_keeps_its_wall_clock_across_both_dst_edges() {
    let tz = TestZone::new_york_2026();
    let spec = weekly(&[0], 9, 0); // Sundays at 09:00.
                                   // The Saturday before the spring-forward Sunday.
    assert_eq!(
        next_occurrence_in(&tz, &spec, at(&tz, 2026, 3, 7, 12, 0)),
        Some(1_772_974_800_000),
    );
    // The Saturday before the fall-back Sunday.
    assert_eq!(
        next_occurrence_in(&tz, &spec, at(&tz, 2026, 10, 31, 12, 0)),
        Some(1_793_541_600_000),
    );
}

#[test]
fn a_skipped_wall_clock_resolves_with_the_offset_from_before_the_gap() {
    // 02:30 on 2026-03-08 does not exist in New York: the clock jumps 02:00 to
    // 03:00. 1772955000000 is 02:30 EST, the pre-transition offset, which
    // renders as 03:30 EDT.
    let ny = TestZone::new_york_2026();
    assert_eq!(
        next_occurrence_in(&ny, &weekly(&[0], 2, 30), at(&ny, 2026, 3, 7, 12, 0)),
        Some(1_772_955_000_000),
    );
    // Sydney's gap is in October and its pre-transition offset is the SMALLER
    // one, so this is the same rule and not a coincidence of sign.
    let syd = TestZone::sydney_2026();
    assert_eq!(
        next_occurrence_in(&syd, &weekly(&[0], 2, 30), at(&syd, 2026, 10, 3, 12, 0)),
        Some(1_791_045_000_000),
    );
}

#[test]
fn an_ambiguous_wall_clock_resolves_to_the_earlier_of_the_two_instants() {
    // 01:30 on 2026-11-01 happens twice in New York. 1793511000000 is the first
    // pass (01:30 EDT); the second is 1793514600000, and answering that would run
    // the schedule an hour late once a year.
    let ny = TestZone::new_york_2026();
    assert_eq!(
        next_occurrence_in(&ny, &weekly(&[0], 1, 30), at(&ny, 2026, 10, 31, 12, 0)),
        Some(1_793_511_000_000),
    );
    // Sydney's fold is in April.
    let syd = TestZone::sydney_2026();
    assert_eq!(
        next_occurrence_in(&syd, &weekly(&[0], 2, 30), at(&syd, 2026, 4, 4, 12, 0)),
        Some(1_775_316_600_000),
    );
}

#[test]
fn a_weekly_on_a_skipped_wall_clock_is_judged_by_the_day_it_landed_on() {
    // The weekday is read off the CONSTRUCTED instant, so a wall clock the zone
    // moved is matched against where it ended up: 02:30 on the spring-forward
    // Sunday lands at 03:30 the same Sunday and a Sunday schedule still matches.
    let ny = TestZone::new_york_2026();
    let saturday = at(&ny, 2026, 3, 7, 12, 0);
    assert_eq!(
        next_occurrence_in(&ny, &weekly(&[0], 2, 30), saturday),
        Some(1_772_955_000_000),
    );
    assert_ne!(
        next_occurrence_in(&ny, &weekly(&[1], 2, 30), saturday),
        Some(1_772_955_000_000),
    );
}

// --- validation --------------------------------------------------------------

#[test]
fn a_weekly_with_no_weekdays_is_refused_and_the_message_says_what_to_do() {
    let err = validate_spec(&weekly(&[], 9, 0)).expect_err("an empty weekday list must be refused");
    assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
    assert!(
        err.message.contains("weekday"),
        "the message must name the field the user has to fix: {}",
        err.message,
    );
    // And it is refused rather than merely never firing, which is the whole
    // defect: `next_occurrence` alone answers None and says nothing.
    let tz = TestZone::fixed(0.0);
    assert_eq!(next_occurrence_in(&tz, &weekly(&[], 9, 0), 0), None);
}

#[test]
fn every_bound_the_contract_bridge_dropped_is_re_asserted() {
    // A constraint survives the bridge only if some Rust type expresses it, and
    // each of these is a schema bound with no Rust type to become.
    let bad = [
        weekly(&[7], 9, 0),
        weekly(&[-1], 9, 0),
        weekly(&[1], 24, 0),
        weekly(&[1], -1, 0),
        weekly(&[1], 9, 60),
        weekly(&[1], 9, -1),
        ScheduleSpec::Monthly {
            day: 32,
            hour: 9,
            minute: 0,
        },
        ScheduleSpec::Monthly {
            day: 0,
            hour: 9,
            minute: 0,
        },
        ScheduleSpec::Yearly {
            month: 13,
            day: 1,
            hour: 9,
            minute: 0,
        },
        ScheduleSpec::Yearly {
            month: 0,
            day: 1,
            hour: 9,
            minute: 0,
        },
        ScheduleSpec::Once { at_ms: 0 },
        ScheduleSpec::Once { at_ms: -1 },
        ScheduleSpec::Once {
            at_ms: 9_007_199_254_740_992,
        },
        ScheduleSpec::Interval { every_minutes: 0 },
        ScheduleSpec::Interval { every_minutes: -1 },
    ];
    for spec in bad {
        let err = validate_spec(&spec).expect_err(&format!("{spec:?} must be refused"));
        assert_eq!(
            err.code,
            crate::error::ErrorCode::ValidationError,
            "{spec:?}"
        );
    }
}

#[test]
fn a_spec_at_every_boundary_is_accepted() {
    // The other half of the guard: a validator that refused everything would
    // pass the test above and break every real schedule.
    let good = [
        weekly(&[0, 6], 0, 0),
        weekly(&[3], 23, 59),
        ScheduleSpec::Monthly {
            day: 1,
            hour: 0,
            minute: 0,
        },
        ScheduleSpec::Monthly {
            day: 31,
            hour: 23,
            minute: 59,
        },
        ScheduleSpec::Yearly {
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
        },
        ScheduleSpec::Yearly {
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
        },
        ScheduleSpec::Once { at_ms: 1 },
        ScheduleSpec::Once {
            at_ms: 9_007_199_254_740_991,
        },
        ScheduleSpec::Interval { every_minutes: 1 },
    ];
    for spec in good {
        validate_spec(&spec).unwrap_or_else(|e| panic!("{spec:?} must be accepted: {e}"));
    }
}

#[test]
fn the_inbound_narrowing_refuses_a_value_that_does_not_fit() {
    use crate::contract::inbound::ScheduleSpec as Inbound;
    // `NonZeroU64` is unbounded above where the wire caps at MAX_SAFE_INTEGER,
    // so the narrowing is where an oversized value is caught rather than wrapping
    // negative. Two mechanisms: `u64::MAX` is refused by `i64::try_from` before
    // the ceiling is consulted, and 2^53 fits an `i64` and is refused ONLY by it.
    for at_ms in [u64::MAX, 9_007_199_254_740_992] {
        let huge = Inbound::Once {
            at_ms: std::num::NonZeroU64::new(at_ms).unwrap(),
        };
        let err = from_inbound(&huge).expect_err("an unsafe integer must be refused");
        assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
    }
    // And the largest value that IS safe still narrows, so a ceiling that
    // refused everything cannot pass.
    assert!(matches!(
        from_inbound(&Inbound::Once {
            at_ms: std::num::NonZeroU64::new(9_007_199_254_740_991).unwrap(),
        }),
        Ok(ScheduleSpec::Once {
            at_ms: 9_007_199_254_740_991
        }),
    ));

    let ok = Inbound::Weekly {
        weekdays: vec![1, 3],
        hour: 9,
        minute: 30,
    };
    match from_inbound(&ok).expect("a well-formed weekly narrows") {
        ScheduleSpec::Weekly {
            weekdays,
            hour,
            minute,
        } => {
            assert_eq!(weekdays, vec![1, 3]);
            assert_eq!((hour, minute), (9, 30));
        }
        other => panic!("expected a weekly, got {other:?}"),
    }
}

// --- fixtures ----------------------------------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::deps::{AutomatedRunner, ScheduledRun, SchedulerDeps};
use super::store::{now_ms, Patch, Store};
use super::Scheduler;
use crate::chat::LoadingProbe;
use crate::contract::api::ScheduledPromptDraft;
use crate::db::Db;
use crate::error::Result;

/// An in-memory database WITH the schema applied. `open_in_memory` deliberately
/// skips the migration, so a fixture that forgets this gets a store whose every
/// read fails.
fn migrated_db() -> Db {
    let db = Db::open_in_memory().unwrap();
    db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();
    db
}

/// `scheduled_prompts.owner_client_id` is a foreign key onto `clients`, and the
/// connection pragmas turn enforcement on, so a fixture needs a real client row.
fn insert_client(db: &Db, id: &str) {
    db.call_blocking(|conn| {
        conn.execute(
            "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms)
             VALUES (?, ?, ?, 0, 0)",
            rusqlite::params![id, format!("{id}-device"), format!("hash-{id}")],
        )?;
        Ok(())
    })
    .unwrap();
}

/// Records what it was asked to fire, and can be told to fail or to read the
/// row back mid-flight.
#[derive(Clone)]
struct Recorder {
    fired: Arc<Mutex<Vec<ScheduledRun>>>,
    fail: bool,
    /// When set, each run reads its own row's `next_run_at_ms` BEFORE returning,
    /// which is how the persist-before-fire ordering becomes assertable rather
    /// than assumed.
    observe: Option<Store>,
    observed: Arc<Mutex<Vec<Option<i64>>>>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            fired: Arc::new(Mutex::new(Vec::new())),
            fail: false,
            observe: None,
            observed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn failing() -> Self {
        Self {
            fail: true,
            ..Self::new()
        }
    }

    fn observing(store: Store) -> Self {
        Self {
            observe: Some(store),
            ..Self::new()
        }
    }
}

#[async_trait]
impl AutomatedRunner for Recorder {
    async fn run(&self, input: ScheduledRun) -> Result<String> {
        if let Some(store) = &self.observe {
            let row = store.by_id(&input.scheduled_prompt_id).await.unwrap();
            self.observed
                .lock()
                .unwrap()
                .push(row.and_then(|r| r.next_run_at_ms));
        }
        let n = {
            let mut fired = self.fired.lock().unwrap();
            fired.push(input);
            fired.len()
        };
        if self.fail {
            return Err(crate::error::internal("the turn could not be started"));
        }
        Ok(format!("session-{n}"))
    }
}

struct Probe(Arc<AtomicBool>);

#[async_trait]
impl LoadingProbe for Probe {
    async fn still_loading(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The row change a [`GapProbe`] is primed with, set after `create` because the
/// plan needs the row's id.
type GapPlan = Arc<Mutex<Option<(String, InTheGap)>>>;

/// What a row does to itself while the tick is parked on the loading probe.
#[derive(Clone, Debug)]
enum InTheGap {
    Disabled,
    AdvancedTo(i64),
}

/// A probe that mutates a row WHILE the tick awaits it.
///
/// Staging the final state does not test the guard at all: `Store::due` already
/// filters on `enabled = 1 AND next_run_at_ms <= now`, so a row changed BEFORE
/// the tick never appears in the due list. The re-read after the probe only
/// matters for a row that was due when the query ran and changed during the
/// await, and the only way to produce that window is from inside the await.
struct GapProbe {
    db: Db,
    plan: GapPlan,
}

#[async_trait]
impl LoadingProbe for GapProbe {
    async fn still_loading(&self) -> bool {
        if let Some((id, what)) = self.plan.lock().unwrap().take() {
            self.db
                .call_blocking(move |conn| {
                    match what {
                        InTheGap::Disabled => conn.execute(
                            "UPDATE scheduled_prompts SET enabled = 0 WHERE id = ?",
                            rusqlite::params![id],
                        )?,
                        InTheGap::AdvancedTo(at) => conn.execute(
                            "UPDATE scheduled_prompts SET next_run_at_ms = ? WHERE id = ?",
                            rusqlite::params![at, id],
                        )?,
                    };
                    Ok(())
                })
                .unwrap();
        }
        // False, so the tick proceeds into the loop rather than deferring. The
        // window this opens is the whole point.
        false
    }
}

struct Env {
    scheduler: Scheduler,
    db: Db,
    runner: Recorder,
    loading: Arc<AtomicBool>,
    owner: String,
    other: String,
}

impl Env {
    fn new() -> Self {
        Self::with_runner(Recorder::new())
    }

    fn with_runner(runner: Recorder) -> Self {
        let db = migrated_db();
        insert_client(&db, "owner");
        insert_client(&db, "other");
        let loading = Arc::new(AtomicBool::new(false));
        let deps = SchedulerDeps::unwired().with_loading(Arc::new(Probe(Arc::clone(&loading))));
        let scheduler = Scheduler::new(db.clone(), deps);
        assert!(
            scheduler.attach_automated(Arc::new(runner.clone())),
            "the first attach must take",
        );
        Self {
            scheduler,
            db,
            runner,
            loading,
            owner: "owner".to_string(),
            other: "other".to_string(),
        }
    }

    /// A scheduler with no runner attached.
    fn unattached() -> Self {
        let db = migrated_db();
        insert_client(&db, "owner");
        insert_client(&db, "other");
        let loading = Arc::new(AtomicBool::new(false));
        let deps = SchedulerDeps::unwired().with_loading(Arc::new(Probe(Arc::clone(&loading))));
        Self {
            scheduler: Scheduler::new(db.clone(), deps),
            db,
            runner: Recorder::new(),
            loading,
            owner: "owner".to_string(),
            other: "other".to_string(),
        }
    }

    fn fired(&self) -> Vec<ScheduledRun> {
        self.runner.fired.lock().unwrap().clone()
    }

    fn set_next_run(&self, id: &str, at: Option<i64>) {
        let id = id.to_string();
        self.db
            .call_blocking(move |conn| {
                conn.execute(
                    "UPDATE scheduled_prompts SET next_run_at_ms = ? WHERE id = ?",
                    rusqlite::params![at, id],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn set_schedule_json(&self, id: &str, json: &str) {
        let id = id.to_string();
        let json = json.to_string();
        self.db
            .call_blocking(move |conn| {
                conn.execute(
                    "UPDATE scheduled_prompts SET schedule_json = ? WHERE id = ?",
                    rusqlite::params![json, id],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn next_run(&self, id: &str) -> Option<i64> {
        let id = id.to_string();
        self.db
            .call_blocking(move |conn| {
                Ok(conn.query_row(
                    "SELECT next_run_at_ms FROM scheduled_prompts WHERE id = ?",
                    [id],
                    |row| row.get::<_, Option<i64>>(0),
                )?)
            })
            .unwrap()
    }

    fn last_run(&self, id: &str) -> Option<i64> {
        let id = id.to_string();
        self.db
            .call_blocking(move |conn| {
                Ok(conn.query_row(
                    "SELECT last_run_at_ms FROM scheduled_prompts WHERE id = ?",
                    [id],
                    |row| row.get::<_, Option<i64>>(0),
                )?)
            })
            .unwrap()
    }
}

/// A schedule that is always some way off, so a test that is not about firing
/// never fires.
fn draft(title: &str) -> ScheduledPromptDraft {
    ScheduledPromptDraft {
        title: title.to_string(),
        instruction: "Summarise my week.".to_string(),
        schedule: weekly(&[1], 9, 0),
        run_missed: false,
    }
}

/// Wait, BOUNDED, for `n` fires. An unbounded drain would hang the whole
/// `--lib` run rather than fail, and take every other package's results with it.
async fn fires_reach(runner: &Recorder, n: usize) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if runner.fired.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok()
}

// --- the store ---------------------------------------------------------------

#[tokio::test]
async fn create_arms_a_future_run_and_crud_stays_owner_scoped() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    assert!(created.enabled);
    assert!(
        created.next_run_at_ms.is_some_and(|at| at > now_ms()),
        "a fresh schedule must be armed at a future instant",
    );
    assert!(created.last_run_at_ms.is_none());

    // The owner sees it.
    assert_eq!(env.scheduler.list(&env.owner).await.unwrap().len(), 1);
    // Nobody else does, through any verb.
    assert!(env.scheduler.list(&env.other).await.unwrap().is_empty());
    for err in [
        env.scheduler.get(&env.other, &created.id).await.err(),
        env.scheduler
            .update(&env.other, &created.id, &Patch::default())
            .await
            .err(),
        env.scheduler.delete(&env.other, &created.id).await.err(),
    ] {
        assert_eq!(
            err.expect("a foreign id must not resolve").code,
            crate::error::ErrorCode::NotFound,
        );
    }
    // And the foreign update did not in fact write.
    assert_eq!(
        env.scheduler.get(&env.owner, &created.id).await.unwrap().id,
        created.id,
    );
}

#[tokio::test]
async fn create_refuses_a_once_whose_time_has_already_passed() {
    let env = Env::new();
    let mut past = draft("Yesterday");
    past.schedule = ScheduleSpec::Once {
        at_ms: now_ms() - 60_000,
    };
    let err = env
        .scheduler
        .create(&env.owner, &past)
        .await
        .expect_err("a passed once can never fire, so it must not be stored");
    assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
    assert!(err.message.contains("past"), "{}", err.message);
    assert!(env.scheduler.list(&env.owner).await.unwrap().is_empty());
}

#[tokio::test]
async fn create_refuses_past_the_per_owner_cap() {
    let env = Env::new();
    for n in 0..100 {
        env.scheduler
            .create(&env.owner, &draft(&format!("schedule {n}")))
            .await
            .unwrap();
    }
    let err = env
        .scheduler
        .create(&env.owner, &draft("one too many"))
        .await
        .expect_err("the 101st must be refused");
    assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
    assert!(err.message.contains("limit"), "{}", err.message);
    // Per OWNER, not per table: another client is unaffected.
    env.scheduler
        .create(&env.other, &draft("theirs"))
        .await
        .unwrap();
}

#[tokio::test]
async fn update_recomputes_the_armed_occurrence_only_when_it_must() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let armed = created.next_run_at_ms.unwrap();

    // A rename must NOT push the schedule forward.
    let renamed = env
        .scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                title: Some("Renamed".to_string()),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.title, "Renamed");
    assert_eq!(renamed.next_run_at_ms, Some(armed));

    // Disabling parks it.
    let disabled = env
        .scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                enabled: Some(false),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.next_run_at_ms, None);

    // Re-enabling re-derives it.
    let reenabled = env
        .scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                enabled: Some(true),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert!(
        reenabled.next_run_at_ms.is_some_and(|at| at > now_ms()),
        "a re-enabled schedule must be armed again",
    );

    // A new schedule re-derives it too. Asserted against a `once`, whose
    // occurrence is exactly its own instant, so this pins the re-derivation
    // rather than comparing two weeklies that can legitimately coincide.
    let fires_at = now_ms() + 3_600_000;
    let rescheduled = env
        .scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                schedule: Some(ScheduleSpec::Once { at_ms: fires_at }),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        rescheduled.next_run_at_ms,
        Some(fires_at),
        "a new schedule must re-derive the occurrence, not keep the old one",
    );
    assert_ne!(rescheduled.next_run_at_ms, reenabled.next_run_at_ms);
}

#[tokio::test]
async fn update_refuses_an_enabled_schedule_that_can_never_fire() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let err = env
        .scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                schedule: Some(ScheduleSpec::Once {
                    at_ms: now_ms() - 60_000,
                }),
                ..Patch::default()
            },
        )
        .await
        .expect_err(
            "an enabled schedule with no occurrence is a dead row the user thinks is armed",
        );
    assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
    // And it did not write: the original schedule survives.
    let after = env.scheduler.get(&env.owner, &created.id).await.unwrap();
    assert!(matches!(after.schedule, ScheduleSpec::Weekly { .. }));
}

// --- the unfireable-schedule guard at all three write paths ------------------

#[tokio::test]
async fn an_empty_weekday_list_is_refused_on_every_path_that_stores_a_schedule() {
    use crate::chat::SchedulePersist;

    let env = Env::new();
    let mut bad = draft("Never fires");
    bad.schedule = weekly(&[], 9, 0);

    // 1. create.
    let err = env
        .scheduler
        .create(&env.owner, &bad)
        .await
        .expect_err("create must refuse");
    assert!(err.message.contains("weekday"), "{}", err.message);

    // 2. update.
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let err = env
        .scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                schedule: Some(weekly(&[], 9, 0)),
                ..Patch::default()
            },
        )
        .await
        .expect_err("update must refuse");
    assert!(err.message.contains("weekday"), "{}", err.message);

    // 3. the agent schedule-confirm path, which reaches the store through
    //    `SchedulePersist` and touches no handler of ours. A guard in a route
    //    would leave exactly this one open, and it is the path the user is
    //    least able to inspect.
    let err = SchedulePersist::create(&env.scheduler, &env.owner, &bad)
        .expect_err("the confirm path must refuse");
    assert!(err.message.contains("weekday"), "{}", err.message);

    // Nothing was stored by any of them beyond the one good schedule.
    let stored = env.scheduler.list(&env.owner).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(matches!(
        stored[0].schedule,
        ScheduleSpec::Weekly { ref weekdays, .. } if !weekdays.is_empty()
    ));

    // And the confirm path DOES store a well-formed draft, so a validator that
    // refused everything cannot pass this test.
    SchedulePersist::create(&env.scheduler, &env.owner, &draft("Confirmed")).unwrap();
    assert_eq!(env.scheduler.list(&env.owner).await.unwrap().len(), 2);
}

// --- the tick ----------------------------------------------------------------

#[tokio::test]
async fn a_due_row_fires_and_its_occurrence_moves_on() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    env.set_next_run(&created.id, Some(now - 1_000));

    env.scheduler.tick_at(now).await;

    let fired = env.fired();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].scheduled_prompt_id, created.id);
    assert_eq!(fired[0].owner_client_id, env.owner);
    assert_eq!(fired[0].instruction, "Summarise my week.");
    assert!(
        env.next_run(&created.id).is_some_and(|at| at > now),
        "the occurrence must move forward, or the next tick fires it again",
    );
    assert_eq!(env.last_run(&created.id), Some(now));
}

#[tokio::test]
async fn the_advanced_occurrence_is_persisted_before_the_turn_starts() {
    // At-most-once. A crash between the write and the turn must skip that
    // occurrence, not repeat it on the next boot, so the row has to be advanced
    // by the time anything observable happens.
    let db = migrated_db();
    insert_client(&db, "owner");
    let runner = Recorder::observing(Store::new(db.clone()));
    let loading = Arc::new(AtomicBool::new(false));
    let scheduler = Scheduler::new(
        db.clone(),
        SchedulerDeps::unwired().with_loading(Arc::new(Probe(Arc::clone(&loading)))),
    );
    scheduler.attach_automated(Arc::new(runner.clone()));

    let created = scheduler
        .create("owner", &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    let id = created.id.clone();
    db.call_blocking(move |conn| {
        conn.execute(
            "UPDATE scheduled_prompts SET next_run_at_ms = ? WHERE id = ?",
            rusqlite::params![now - 1_000, id],
        )?;
        Ok(())
    })
    .unwrap();

    scheduler.tick_at(now).await;

    let observed = runner.observed.lock().unwrap().clone();
    assert_eq!(observed.len(), 1);
    assert!(
        observed[0].is_some_and(|at| at > now),
        "the runner saw {observed:?}, so the advance had not landed when the turn started",
    );
}

#[tokio::test]
async fn a_fire_that_fails_is_logged_and_leaves_the_row_advanced() {
    let env = Env::with_runner(Recorder::failing());
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    env.set_next_run(&created.id, Some(now - 1_000));

    env.scheduler.tick_at(now).await;

    assert_eq!(env.fired().len(), 1, "it was attempted");
    assert!(
        env.next_run(&created.id).is_some_and(|at| at > now),
        "a failed turn must not leave the row due, or it retries forever",
    );
}

#[tokio::test]
async fn a_missed_row_is_skipped_unless_it_says_to_make_it_up() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    // Past the grace window: missed, not merely late.
    env.set_next_run(&created.id, Some(now - super::MISSED_GRACE_MS - 1));

    env.scheduler.tick_at(now).await;
    assert!(
        env.fired().is_empty(),
        "run-missed is off, so it must not fire"
    );
    assert!(
        env.next_run(&created.id).is_some_and(|at| at > now),
        "a skipped occurrence is still consumed, or it is re-skipped forever",
    );
    assert_eq!(
        env.last_run(&created.id),
        None,
        "nothing ran, so nothing may be recorded as having run",
    );

    // The same row with the flag on DOES fire.
    env.scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                run_missed: Some(true),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    env.set_next_run(&created.id, Some(now - super::MISSED_GRACE_MS - 1));
    env.scheduler.tick_at(now).await;
    assert_eq!(env.fired().len(), 1);
}

#[tokio::test]
async fn a_late_row_inside_the_grace_window_always_fires() {
    // The other side of the missed threshold: late is not missed, and a late
    // row fires whatever `runMissed` says.
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    env.set_next_run(&created.id, Some(now - super::MISSED_GRACE_MS));

    env.scheduler.tick_at(now).await;
    assert_eq!(
        env.fired().len(),
        1,
        "run-missed is off, but this is late, not missed"
    );
}

#[tokio::test]
async fn a_fired_once_goes_dormant_rather_than_re_arming() {
    let env = Env::new();
    let fires_at = now_ms() + 60_000;
    let mut once = draft("One off");
    once.schedule = ScheduleSpec::Once { at_ms: fires_at };
    let created = env.scheduler.create(&env.owner, &once).await.unwrap();
    assert_eq!(created.next_run_at_ms, Some(fires_at));
    // Tick from AFTER the instant, which is the only way a `once` is ever due.
    let now = fires_at + 1_000;

    env.scheduler.tick_at(now).await;

    assert_eq!(env.fired().len(), 1);
    assert_eq!(
        env.next_run(&created.id),
        None,
        "a passed once has no next occurrence, so the row parks",
    );
    // And it stays parked: a second tick does nothing.
    env.scheduler.tick_at(now).await;
    assert_eq!(env.fired().len(), 1);
    // The client tells dormant from armed by the key being ABSENT, not null.
    let prompt = env.scheduler.get(&env.owner, &created.id).await.unwrap();
    let json = serde_json::to_value(&prompt).unwrap();
    assert!(
        json.get("nextRunAtMs").is_none(),
        "an absent occurrence must be omitted, not sent as null: {json}",
    );
    assert!(json.get("lastRunAtMs").is_some(), "{json}");
}

#[tokio::test]
async fn an_unreadable_schedule_parks_its_row_and_the_others_still_fire() {
    let env = Env::new();
    let broken = env
        .scheduler
        .create(&env.owner, &draft("Broken"))
        .await
        .unwrap();
    let healthy = env
        .scheduler
        .create(&env.owner, &draft("Healthy"))
        .await
        .unwrap();
    let now = now_ms();
    env.set_schedule_json(&broken.id, "{\"kind\":\"fortnightly\"}");
    env.set_next_run(&broken.id, Some(now - 1_000));
    env.set_next_run(&healthy.id, Some(now - 1_000));

    env.scheduler.tick_at(now).await;

    assert_eq!(
        env.next_run(&broken.id),
        None,
        "a schedule that cannot be read must be parked, not retried forever",
    );
    let fired = env.fired();
    assert_eq!(
        fired.len(),
        1,
        "the broken row must not wedge the healthy one"
    );
    assert_eq!(fired[0].scheduled_prompt_id, healthy.id);
}

#[tokio::test]
async fn a_loading_model_defers_every_due_row_and_asks_for_a_retry() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    env.set_next_run(&created.id, Some(now - 1_000));
    env.loading.store(true, Ordering::SeqCst);

    env.scheduler.tick_at(now).await;

    assert!(
        env.fired().is_empty(),
        "a turn fired mid-load races it and errors"
    );
    assert_eq!(
        env.next_run(&created.id),
        Some(now - 1_000),
        "a deferred row must stay due, or the occurrence is silently lost",
    );
    assert_eq!(
        env.scheduler.arm_delay().await,
        Some(Duration::from_millis(super::SIDECAR_RETRY_MS)),
        "the tick must ask for the short retry rather than re-arming from the rows",
    );

    // Once the model is up, the held row fires.
    env.loading.store(false, Ordering::SeqCst);
    env.scheduler.tick_at(now).await;
    assert_eq!(env.fired().len(), 1);
}

/// A scheduler whose loading probe changes the row mid-await.
///
/// Built here rather than in `Env` because the plan needs the row's id, which
/// only exists after `create`.
fn gap_env() -> (Env, GapPlan) {
    let db = migrated_db();
    insert_client(&db, "owner");
    insert_client(&db, "other");
    let plan = Arc::new(Mutex::new(None));
    let runner = Recorder::new();
    let scheduler = Scheduler::new(
        db.clone(),
        SchedulerDeps::unwired().with_loading(Arc::new(GapProbe {
            db: db.clone(),
            plan: Arc::clone(&plan),
        })),
    );
    assert!(scheduler.attach_automated(Arc::new(runner.clone())));
    let env = Env {
        scheduler,
        db,
        runner,
        loading: Arc::new(AtomicBool::new(false)),
        owner: "owner".to_string(),
        other: "other".to_string(),
    };
    (env, plan)
}

#[tokio::test]
async fn a_row_disabled_while_the_probe_was_awaited_is_not_fired() {
    // The re-read after the loading probe, through the window it exists for: the
    // row is due when `due()` runs and is disabled while the tick is parked.
    let (env, plan) = gap_env();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    env.set_next_run(&created.id, Some(now - 1_000));
    *plan.lock().unwrap() = Some((created.id.clone(), InTheGap::Disabled));

    env.scheduler.tick_at(now).await;

    assert!(
        env.fired().is_empty(),
        "a row disabled during the await must not fire: acting on the pre-await \
         snapshot would run an instruction the user had just switched off",
    );
}

#[tokio::test]
async fn a_row_advanced_while_the_probe_was_awaited_is_not_fired_again() {
    // What a concurrent `run_now`, or a second tick, leaves behind.
    let (env, plan) = gap_env();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    env.set_next_run(&created.id, Some(now - 1_000));
    *plan.lock().unwrap() = Some((created.id.clone(), InTheGap::AdvancedTo(now + 60_000)));

    env.scheduler.tick_at(now).await;

    assert!(
        env.fired().is_empty(),
        "an occurrence advanced during the await is no longer due, and firing it \
         would duplicate the turn",
    );
    assert_eq!(
        env.next_run(&created.id),
        Some(now + 60_000),
        "and the freshly-advanced occurrence must not be clobbered",
    );
}

#[tokio::test]
async fn a_row_that_is_still_due_after_the_await_does_fire() {
    // The control. Without it, a tick that refused to fire ANYTHING would pass
    // both tests above.
    let (env, plan) = gap_env();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let now = now_ms();
    env.set_next_run(&created.id, Some(now - 1_000));
    assert!(plan.lock().unwrap().is_none());

    env.scheduler.tick_at(now).await;
    assert_eq!(env.fired().len(), 1);
}

// --- run now -----------------------------------------------------------------

#[tokio::test]
async fn run_now_fires_immediately_and_leaves_the_armed_occurrence_alone() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    let armed = created.next_run_at_ms.unwrap();

    let session = env
        .scheduler
        .run_now(&env.owner, &created.id)
        .await
        .unwrap();
    assert_eq!(session, "session-1");
    assert_eq!(env.fired().len(), 1);
    assert_eq!(
        env.next_run(&created.id),
        Some(armed),
        "\"Run Now\" is extra, not instead of",
    );
    assert!(env.last_run(&created.id).is_some());
}

#[tokio::test]
async fn run_now_refuses_a_disabled_schedule() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    env.scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                enabled: Some(false),
                ..Patch::default()
            },
        )
        .await
        .unwrap();

    let err = env
        .scheduler
        .run_now(&env.owner, &created.id)
        .await
        .expect_err("a disabled schedule must not run");
    assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
    assert!(env.fired().is_empty());
}

#[tokio::test]
async fn run_now_refuses_while_the_local_model_is_loading() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    env.loading.store(true, Ordering::SeqCst);

    let err = env
        .scheduler
        .run_now(&env.owner, &created.id)
        .await
        .expect_err("refusing loudly beats handing back a session that errors on its own");
    assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
    assert!(err.message.contains("loading"), "{}", err.message);
    assert!(env.fired().is_empty());
    assert_eq!(env.last_run(&created.id), None);
}

#[tokio::test]
async fn a_scheduler_with_no_runner_refuses_rather_than_quietly_doing_nothing() {
    let env = Env::unattached();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();

    let err = env
        .scheduler
        .run_now(&env.owner, &created.id)
        .await
        .expect_err("reporting a session that was never created is the failure to avoid");
    assert_eq!(err.code, crate::error::ErrorCode::ServerUnavailable);

    // And a due tick reports rather than silently consuming the occurrence
    // as though it had run.
    let now = now_ms();
    env.set_next_run(&created.id, Some(now - 1_000));
    env.scheduler.tick_at(now).await;
    assert!(env.next_run(&created.id).is_some_and(|at| at > now));
}

#[tokio::test]
async fn attaching_a_runner_twice_does_not_replace_the_live_one() {
    let env = Env::new();
    assert!(
        !env.scheduler.attach_automated(Arc::new(Recorder::new())),
        "a second attach must be refused, or a stray wiring silently takes over",
    );
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    env.scheduler
        .run_now(&env.owner, &created.id)
        .await
        .unwrap();
    assert_eq!(
        env.fired().len(),
        1,
        "the FIRST runner is still the live one"
    );
}

// --- the timer ---------------------------------------------------------------
//
// Every wait here is bounded. A poll loop with no timeout does not fail when the
// thing never arrives, it HANGS, which wedges the whole `--lib` run and masks
// every other package's result.

#[tokio::test]
async fn an_empty_table_parks_the_timer_and_a_later_row_still_wakes_it() {
    let env = Env::new();
    env.scheduler.start();

    // Nothing scheduled: `arm_delay` is None and the loop sits on
    // `future::pending()`. Proven as a state rather than assumed.
    assert_eq!(env.scheduler.arm_delay().await, None);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(env.fired().is_empty());

    // And the parked loop is alive: a row inserted now wakes it.
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    env.set_next_run(&created.id, Some(now_ms() - 1_000));
    env.scheduler
        .create(&env.owner, &draft("poke"))
        .await
        .unwrap();

    assert!(
        fires_reach(&env.runner, 1).await,
        "the parked timer never woke, so nothing would ever fire on this core",
    );
    env.scheduler.shutdown();
}

#[tokio::test]
async fn start_catches_up_a_row_the_core_was_off_for() {
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    // Overdue by an hour, which is what a core that was off comes back to.
    env.set_next_run(&created.id, Some(now_ms() - 3_600_000));
    // Inside the grace window it would be merely late; past it, only
    // `runMissed` makes it fire, which is what a catch-up is.
    env.scheduler
        .update(
            &env.owner,
            &created.id,
            &Patch {
                run_missed: Some(true),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    env.set_next_run(&created.id, Some(now_ms() - 3_600_000));

    env.scheduler.start();
    assert!(
        fires_reach(&env.runner, 1).await,
        "an overdue row must fire on the first tick: that IS the boot catch-up",
    );
    env.scheduler.shutdown();
}

#[tokio::test]
async fn shutdown_stops_a_loop_that_is_parked_on_a_deadline() {
    // The loop is parked on a real sleep with a row due at the end and nothing
    // pokes it afterwards, so the only way out is the cancellation branch of the
    // `select!` rather than the loop-top guard.
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    env.set_next_run(&created.id, Some(now_ms() + 400));
    env.scheduler.start();
    // Long enough for the loop to have computed its delay and parked on it.
    tokio::time::sleep(Duration::from_millis(120)).await;

    env.scheduler.shutdown();

    // Past the occurrence, with margin. A loop that did not stop fires here.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        env.fired().is_empty(),
        "a stopped scheduler must not fire a turn into a core that is closing",
    );
}

#[tokio::test]
async fn the_same_schedule_does_fire_when_nothing_stops_it() {
    // The control for the test above. Without it, a scheduler that never fired
    // at all would pass that one, and the shutdown assertion would be worthless.
    let env = Env::new();
    let created = env
        .scheduler
        .create(&env.owner, &draft("Weekly review"))
        .await
        .unwrap();
    env.set_next_run(&created.id, Some(now_ms() + 400));
    env.scheduler.start();

    assert!(
        fires_reach(&env.runner, 1).await,
        "the armed occurrence never fired, so the shutdown test proves nothing",
    );
    env.scheduler.shutdown();
}

// --- the route slice ---------------------------------------------------------
//
// These drive `super::routes::routes`, the production slice function, which
// applies its own `require_client` layer: deleting that line makes the auth test
// go red. They deliberately do not cover the `.merge(...)` in
// `transport/router.rs`, which nothing inside this module can assert.

fn route_env() -> (crate::transport::state::AppContext, Env) {
    let env = Env::new();
    let ctx = crate::transport::testing::context_with(Arc::new(
        crate::transport::testing::StubAuth::new().with_client("t0ken", "owner"),
    ));
    (ctx, env)
}

/// One request through a freshly built slice. `token` absent means no bearer.
async fn route_call(
    ctx: &crate::transport::state::AppContext,
    env: &Env,
    token: Option<&str>,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = super::routes::routes(ctx, env.scheduler.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, value)
}

/// The same, as the paired owner.
async fn call(
    ctx: &crate::transport::state::AppContext,
    env: &Env,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    route_call(ctx, env, Some("t0ken"), method, uri, body).await
}

#[tokio::test]
async fn every_route_needs_a_bearer() {
    let (ctx, env) = route_env();
    let cases = [
        ("GET", "/api/v1/scheduled-prompts"),
        ("POST", "/api/v1/scheduled-prompts"),
        ("PATCH", "/api/v1/scheduled-prompts/abc"),
        ("DELETE", "/api/v1/scheduled-prompts/abc"),
        ("POST", "/api/v1/scheduled-prompts/abc/run"),
    ];
    for (method, uri) in cases {
        let (status, _) =
            route_call(&ctx, &env, None, method, uri, Some(serde_json::json!({}))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} answered an unauthenticated caller",
        );
    }
}

#[tokio::test]
async fn the_slice_round_trips_a_schedule() {
    let (ctx, env) = route_env();
    let body = serde_json::json!({
        "title": "Weekly review",
        "instruction": "Summarise my week.",
        "schedule": { "kind": "weekly", "weekdays": [1], "hour": 9, "minute": 0 },
        "runMissed": false,
    });
    let (status, created) = call(&ctx, &env, "POST", "/api/v1/scheduled-prompts", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["ownerClientId"], "owner");
    assert!(created["nextRunAtMs"].is_i64());

    let (_, listed) = call(&ctx, &env, "GET", "/api/v1/scheduled-prompts", None).await;
    assert_eq!(listed["scheduledPrompts"].as_array().unwrap().len(), 1);

    let (status, deleted) = call(
        &ctx,
        &env,
        "DELETE",
        &format!("/api/v1/scheduled-prompts/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["ok"], true);
    assert!(env.scheduler.list("owner").await.unwrap().is_empty());
}

#[tokio::test]
async fn the_create_route_refuses_a_weekly_with_no_weekdays() {
    // The unfireable-schedule case at the wire, where a user meets it.
    let (ctx, env) = route_env();
    let body = serde_json::json!({
        "title": "Never fires",
        "instruction": "Summarise my week.",
        "schedule": { "kind": "weekly", "weekdays": [], "hour": 9, "minute": 0 },
        "runMissed": false,
    });
    let (status, body) = call(&ctx, &env, "POST", "/api/v1/scheduled-prompts", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "validation_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("weekday"),
        "{body}",
    );
    assert!(env.scheduler.list("owner").await.unwrap().is_empty());
}

#[tokio::test]
async fn the_patch_route_is_strict_and_refuses_an_empty_title() {
    // The patch body is a route-local schema the contract bridge never sees, so
    // both of these are enforced here or nowhere.
    let (ctx, env) = route_env();
    let created = env
        .scheduler
        .create("owner", &draft("Weekly review"))
        .await
        .unwrap();

    for (label, body) in [
        ("an unknown field", serde_json::json!({ "nope": 1 })),
        ("an empty title", serde_json::json!({ "title": "" })),
        (
            "an empty instruction",
            serde_json::json!({ "instruction": "" }),
        ),
    ] {
        let (status, _) = call(
            &ctx,
            &env,
            "PATCH",
            &format!("/api/v1/scheduled-prompts/{}", created.id),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label} was accepted");
    }
}
