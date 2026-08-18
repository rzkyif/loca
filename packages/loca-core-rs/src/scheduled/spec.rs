//! The calendar: when does a [`ScheduleSpec`] next fire, and is it one a user
//! can be told the truth about.
//!
//! Two pure functions and one conversion. No database, no clock, no timezone
//! other than the one handed in.
//!
//! # Local time is the specification, not an implementation detail
//!
//! Every candidate is built from a wall-clock date and time and then resolved
//! against a timezone, never by adding 86_400_000 milliseconds per day. A 09:00
//! weekly therefore stays 09:00 local across a DST transition, on a day that is
//! 23 or 25 hours long. `services/prompt-scheduler.test.ts:131-156` exists in
//! the oracle for exactly that reason, and this module keeps the property for
//! the same one.
//!
//! [`next_occurrence_in`] takes the timezone as a parameter so a test can drive
//! a zone it controls. The alternative, setting `TZ` on the process, is a
//! process-global mutation under a threaded test runner, and this crate has
//! already lost a day to one of those.

use chrono::{Datelike, Local, LocalResult, NaiveDate, Offset, TimeDelta, TimeZone};

use crate::contract::api::ScheduleSpec;
use crate::contract::inbound::ScheduleSpec as InboundScheduleSpec;
use crate::error::{validation, Result};

/// JavaScript's `Number.MAX_SAFE_INTEGER`, which is the upper bound Zod puts on
/// every millisecond and minute field (`z.number().int()`).
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Re-assert every bound the contract bridge dropped on the way to Rust.
///
/// **D-083 and D-095.** A constraint survives the bridge only if some Rust type
/// expresses it: `minimum: 1` becomes `NonZeroU64` and lives, `maximum: 23` has
/// no type to become and dies silently. Seven constraints on this one type
/// evaporate, and one of them is not cosmetic: a `weekly` spec with an empty
/// `weekdays` list matches no day, so it is accepted, stored, shown to the user
/// with a `nextRunAtMs`, and then never fires.
///
/// Called from the STORE rather than from a route handler, and that placement
/// is load-bearing. There are three write paths into `scheduled_prompts` and
/// only the store is on all three: `POST`, `PATCH`, and the agent
/// schedule-confirm flow, which reaches
/// [`SchedulePersist::create`](crate::chat::SchedulePersist::create) without
/// passing through any handler of ours.
pub fn validate_spec(spec: &ScheduleSpec) -> Result<()> {
    match spec {
        ScheduleSpec::Once { at_ms } => {
            if *at_ms <= 0 || *at_ms > MAX_SAFE_INTEGER {
                return Err(validation("the scheduled time is not a valid instant"));
            }
        }
        ScheduleSpec::Interval { every_minutes } => {
            if *every_minutes < 1 || *every_minutes > MAX_SAFE_INTEGER {
                return Err(validation("the interval must be at least one minute"));
            }
        }
        ScheduleSpec::Weekly {
            weekdays,
            hour,
            minute,
        } => {
            // The D-083 guard. The message says what to do, because the whole
            // point of the defect is that the user currently gets silence.
            if weekdays.is_empty() {
                return Err(validation(
                    "a weekly schedule needs at least one weekday; pick one",
                ));
            }
            if weekdays.iter().any(|d| !(0..=6).contains(d)) {
                return Err(validation(
                    "a weekday must be 0 (Sunday) through 6 (Saturday)",
                ));
            }
            check_time(*hour, *minute)?;
        }
        ScheduleSpec::Monthly { day, hour, minute } => {
            check_day(*day)?;
            check_time(*hour, *minute)?;
        }
        ScheduleSpec::Yearly {
            month,
            day,
            hour,
            minute,
        } => {
            if !(1..=12).contains(month) {
                return Err(validation("a month must be 1 through 12"));
            }
            check_day(*day)?;
            check_time(*hour, *minute)?;
        }
    }
    Ok(())
}

fn check_time(hour: i64, minute: i64) -> Result<()> {
    if !(0..=23).contains(&hour) {
        return Err(validation("an hour must be 0 through 23"));
    }
    if !(0..=59).contains(&minute) {
        return Err(validation("a minute must be 0 through 59"));
    }
    Ok(())
}

fn check_day(day: i64) -> Result<()> {
    if !(1..=31).contains(&day) {
        return Err(validation("a day of the month must be 1 through 31"));
    }
    Ok(())
}

/// The soonest occurrence of `spec` strictly after `after_ms`, in the core's
/// local time, or `None` when there is none.
///
/// Only a passed `once` can genuinely yield `None` for a spec that
/// [`validate_spec`] accepts. An empty `weekdays` list also yields `None`, but
/// the store refuses that first, so a caller never observes it: refusing there
/// lets the user be told about the weekdays instead of being told the time is
/// in the past, which would be false.
pub fn next_occurrence(spec: &ScheduleSpec, after_ms: i64) -> Option<i64> {
    next_occurrence_in(&Local, spec, after_ms)
}

/// [`next_occurrence`] against an explicit timezone.
pub fn next_occurrence_in<Tz: TimeZone>(
    tz: &Tz,
    spec: &ScheduleSpec,
    after_ms: i64,
) -> Option<i64> {
    match spec {
        ScheduleSpec::Once { at_ms } => (*at_ms > after_ms).then_some(*at_ms),
        ScheduleSpec::Interval { every_minutes } => {
            after_ms.checked_add(every_minutes.checked_mul(60_000)?)
        }
        ScheduleSpec::Weekly {
            weekdays,
            hour,
            minute,
        } => {
            let after = local_date(tz, after_ms)?;
            // Same wall-clock time each day, so candidates ascend with the day
            // offset and the first match is the soonest. Seven offsets covers a
            // full week; the eighth is the same weekday as offset zero, which
            // matters when today's occurrence has already passed.
            for offset in 0..=7u64 {
                let date = after.checked_add_days(chrono::Days::new(offset))?;
                let Some(at) = resolve(tz, date, *hour, *minute) else {
                    continue;
                };
                // The weekday of the RESOLVED instant, not of the naive date.
                // `candidate.getDay()` in the oracle reads the constructed
                // `Date`, so a wall clock the zone skipped is judged by where it
                // landed rather than by where it was asked for.
                let (weekday, ms) = at;
                if !weekdays.contains(&weekday) {
                    continue;
                }
                if ms > after_ms {
                    return Some(ms);
                }
            }
            None
        }
        ScheduleSpec::Monthly { day, hour, minute } => {
            let after = local_date(tz, after_ms)?;
            for offset in 0..=1i32 {
                let (year, month0) = normalise_month(after.year(), after.month0() as i32 + offset);
                let clamped = (*day).min(days_in_month(year, month0) as i64);
                let date = NaiveDate::from_ymd_opt(year, month0 as u32 + 1, clamped as u32)?;
                if let Some((_, ms)) = resolve(tz, date, *hour, *minute) {
                    if ms > after_ms {
                        return Some(ms);
                    }
                }
            }
            None
        }
        ScheduleSpec::Yearly {
            month,
            day,
            hour,
            minute,
        } => {
            let after = local_date(tz, after_ms)?;
            for offset in 0..=1i32 {
                let year = after.year() + offset;
                let month0 = *month as i32 - 1;
                let clamped = (*day).min(days_in_month(year, month0) as i64);
                let date = NaiveDate::from_ymd_opt(year, *month as u32, clamped as u32)?;
                if let Some((_, ms)) = resolve(tz, date, *hour, *minute) {
                    if ms > after_ms {
                        return Some(ms);
                    }
                }
            }
            None
        }
    }
}

/// The local calendar date `at_ms` falls on.
fn local_date<Tz: TimeZone>(tz: &Tz, at_ms: i64) -> Option<NaiveDate> {
    Some(tz.timestamp_millis_opt(at_ms).single()?.date_naive())
}

/// Resolve a wall-clock date and time in `tz`, answering the resolved weekday
/// (0 = Sunday, matching `Date.getDay()`) and the instant in milliseconds.
///
/// # The two transition cases, measured rather than reasoned about
///
/// ECMAScript's `UTC(t)` uses the timezone offset in effect BEFORE a transition
/// for both a skipped and a repeated local time, and V8 was measured to agree in
/// both hemispheres before this was written:
///
/// ```text
/// TZ=America/New_York   new Date(2026, 2, 8, 2, 30) -> 1772955000000  (02:30 EST, renders 03:30 EDT)
/// TZ=America/New_York   new Date(2026,10, 1, 1, 30) -> 1793511000000  (01:30 EDT, the earlier of two)
/// TZ=Australia/Sydney   new Date(2026, 9, 4, 2, 30) -> 1791045000000  (02:30 AEST, renders 03:30 AEDT)
/// TZ=Australia/Sydney   new Date(2026, 3, 5, 2, 30) -> 1775316600000  (02:30 AEDT, the earlier of two)
/// ```
///
/// At a fold the offset shrinks, so the pre-transition offset is the EARLIER of
/// the two instants: `LocalResult::Ambiguous(earliest, _)`. At a gap the offset
/// grows, so the pre-transition offset pushes the wall clock forward, and
/// `chrono` reports `LocalResult::None` without handing back an offset to use.
/// Walking backwards until a local time resolves recovers it, and a
/// half-hour step is what makes that correct for Lord Howe's 30-minute gap as
/// well as the usual hour.
fn resolve<Tz: TimeZone>(tz: &Tz, date: NaiveDate, hour: i64, minute: i64) -> Option<(i64, i64)> {
    let naive = date.and_hms_opt(hour.try_into().ok()?, minute.try_into().ok()?, 0)?;
    let offset = match tz.offset_from_local_datetime(&naive) {
        LocalResult::Single(offset) => offset,
        LocalResult::Ambiguous(earliest, _) => earliest,
        LocalResult::None => {
            let mut found = None;
            for back in 1..=48i64 {
                let probe = naive.checked_sub_signed(TimeDelta::minutes(30 * back))?;
                match tz.offset_from_local_datetime(&probe) {
                    LocalResult::Single(offset) => {
                        found = Some(offset);
                        break;
                    }
                    LocalResult::Ambiguous(earliest, _) => {
                        found = Some(earliest);
                        break;
                    }
                    LocalResult::None => continue,
                }
            }
            found?
        }
    };
    let shift = TimeDelta::try_seconds(offset.fix().local_minus_utc().into())?;
    let instant = naive.and_utc().checked_sub_signed(shift)?;
    let ms = instant.timestamp_millis();
    // Read the weekday back off the resolved instant, which is what makes a
    // skipped wall clock report the day it actually landed on.
    let local = tz.timestamp_millis_opt(ms).single()?;
    Some((i64::from(local.weekday().num_days_from_sunday()), ms))
}

/// Carry an out-of-range month index into the following year.
///
/// The oracle leans on `Date`'s own normalisation here: `daysInMonth` is
/// `new Date(year, monthIndex + 1, 0).getDate()`, which only answers correctly
/// for December because month index 12 means January of the next year. Doing it
/// explicitly rather than hoping a chrono constructor overflows the same way.
fn normalise_month(year: i32, month0: i32) -> (i32, i32) {
    (year + month0.div_euclid(12), month0.rem_euclid(12))
}

/// Days in a month, by zero-based month index, normalising an out-of-range one.
fn days_in_month(year: i32, month0: i32) -> u32 {
    let (year, month0) = normalise_month(year, month0);
    match month0 {
        0 | 2 | 4 | 6 | 7 | 9 | 11 => 31,
        3 | 5 | 8 | 10 => 30,
        _ => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// The Zod-generated inbound spec narrowed to the domain shape everything else
/// speaks.
///
/// Both directions of the contract split meet here: a route body deserialises
/// as [`InboundScheduleSpec`], which enforces the constraints typify DID carry
/// (`NonZeroU64` for the four positive fields), and everything downstream, the
/// store, the persisted JSON, and
/// [`SchedulePersist`](crate::chat::SchedulePersist), speaks
/// [`ScheduleSpec`]. The narrowing is fallible because `NonZeroU64` is
/// unbounded above where the oracle caps at `MAX_SAFE_INTEGER`, so the value
/// can genuinely not fit.
pub fn from_inbound(spec: &InboundScheduleSpec) -> Result<ScheduleSpec> {
    let narrowed = match spec {
        InboundScheduleSpec::Once { at_ms } => ScheduleSpec::Once {
            at_ms: fits(at_ms.get(), "the scheduled time")?,
        },
        InboundScheduleSpec::Interval { every_minutes } => ScheduleSpec::Interval {
            every_minutes: fits(every_minutes.get(), "the interval")?,
        },
        InboundScheduleSpec::Weekly {
            weekdays,
            hour,
            minute,
        } => ScheduleSpec::Weekly {
            weekdays: weekdays.clone(),
            hour: *hour,
            minute: *minute,
        },
        InboundScheduleSpec::Monthly { day, hour, minute } => ScheduleSpec::Monthly {
            day: fits(day.get(), "the day of the month")?,
            hour: *hour,
            minute: *minute,
        },
        InboundScheduleSpec::Yearly {
            month,
            day,
            hour,
            minute,
        } => ScheduleSpec::Yearly {
            month: fits(month.get(), "the month")?,
            day: fits(day.get(), "the day of the month")?,
            hour: *hour,
            minute: *minute,
        },
    };
    Ok(narrowed)
}

fn fits(value: u64, what: &str) -> Result<i64> {
    i64::try_from(value)
        .ok()
        .filter(|v| *v <= MAX_SAFE_INTEGER)
        .ok_or_else(|| validation(format!("{what} is out of range")))
}
