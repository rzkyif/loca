//! The calendar: when a [`ScheduleSpec`] next fires, and spec validation.
//!
//! Candidates are wall-clock dates resolved against a timezone, never
//! `after + 86_400_000`, so a 09:00 weekly stays 09:00 local across DST.
//! The zone is a parameter so tests never mutate the process-global `TZ`.

use chrono::{Datelike, Local, LocalResult, NaiveDate, Offset, TimeDelta, TimeZone};

use crate::contract::api::ScheduleSpec;
use crate::contract::inbound::ScheduleSpec as InboundScheduleSpec;
use crate::error::{validation, Result};

/// JavaScript's `Number.MAX_SAFE_INTEGER`, the contract's bound on integer fields.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Re-assert the bounds the generated contract types cannot express (an empty
/// `weekdays` list would be stored and never fire). Called from the store, the
/// only point on all three write paths (POST, PATCH, schedule-confirm).
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
/// local time. Only a passed `once` yields `None` for a validated spec.
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
            // Candidates ascend with the offset; the eighth day is offset zero's
            // weekday again, for when today's occurrence has passed.
            for offset in 0..=7u64 {
                let date = after.checked_add_days(chrono::Days::new(offset))?;
                let Some(at) = resolve(tz, date, *hour, *minute) else {
                    continue;
                };
                // Weekday of the RESOLVED instant: a skipped wall clock is
                // judged by where it landed.
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
/// Matches ECMAScript, which uses the offset in effect BEFORE a DST transition:
/// the earlier instant at a fold, and at a gap the offset recovered by walking
/// back in half-hour steps (Lord Howe has a 30-minute gap).
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
    let local = tz.timestamp_millis_opt(ms).single()?;
    Some((i64::from(local.weekday().num_days_from_sunday()), ms))
}

/// Carry an out-of-range month index into the following year.
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

/// The inbound spec narrowed to the domain shape. Fallible because `NonZeroU64`
/// is unbounded above where the contract caps at `MAX_SAFE_INTEGER`.
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
