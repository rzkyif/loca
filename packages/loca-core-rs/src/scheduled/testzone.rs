//! A timezone the tests own, so DST is exercised without touching `TZ`.
//!
//! `chrono::Local` reads the process environment, and mutating `TZ` from a test
//! is a process-global write under a threaded runner: every other test in the
//! binary sees it, and the ordering is not something a test can control. This
//! crate has already lost a day to one process-global re-entry (D-085), so
//! [`next_occurrence_in`](super::next_occurrence_in) takes the zone as a
//! parameter and this is what the tests pass.
//!
//! It is a COLLABORATOR, not a copy of the subject (D-084): every assertion
//! runs through the production function, which is the shipped one. Only the
//! zone it resolves against is substituted.
//!
//! Two instances model the 2026 transitions in each hemisphere, which is what
//! shows the resolution rule is about the TRANSITION rather than about the
//! month.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{FixedOffset, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone};

fn east(hours: f64) -> FixedOffset {
    FixedOffset::east_opt((hours * 3600.0) as i32).expect("a whole-hour offset is in range")
}

/// A zone described the way the tz database describes one: an initial offset
/// and an ascending list of instants at which it changes.
#[derive(Clone, Debug)]
pub struct TestZone {
    initial: FixedOffset,
    /// `(utc seconds, the offset in force from that instant on)`, ascending.
    changes: Vec<(i64, FixedOffset)>,
}

impl TestZone {
    /// `America/New_York` across 2026. EST is UTC-5, EDT is UTC-4; the gap
    /// opens 2026-03-08 02:00 local and the fold closes 2026-11-01 02:00 local.
    pub fn new_york_2026() -> Self {
        Self {
            initial: east(-5.0),
            changes: vec![
                (1_772_953_200, east(-4.0)), // 2026-03-08T07:00:00Z
                (1_793_512_800, east(-5.0)), // 2026-11-01T06:00:00Z
            ],
        }
    }

    /// `Australia/Sydney` across 2026, where the two transitions land in the
    /// opposite months. AEST is UTC+10, AEDT is UTC+11, and daylight time is in
    /// force at the start of the year.
    pub fn sydney_2026() -> Self {
        Self {
            initial: east(11.0),
            changes: vec![
                (1_775_318_400, east(10.0)), // 2026-04-04T16:00:00Z
                (1_791_043_200, east(11.0)), // 2026-10-03T16:00:00Z
            ],
        }
    }

    /// A zone with no transitions at all, for the cases that are about the
    /// calendar rather than about DST.
    pub fn fixed(hours: f64) -> Self {
        Self {
            initial: east(hours),
            changes: Vec::new(),
        }
    }

    fn offset_at(&self, utc_secs: i64) -> FixedOffset {
        self.changes
            .iter()
            .rev()
            .find(|(at, _)| utc_secs >= *at)
            .map_or(self.initial, |(_, offset)| *offset)
    }

    /// An offset is a valid reading of `local` only if the instant it implies
    /// is one where that offset is actually in force. Zero valid readings is a
    /// gap; two is a fold.
    fn candidates(&self, local: &NaiveDateTime) -> Vec<FixedOffset> {
        let mut out: Vec<FixedOffset> = Vec::new();
        let mut all = vec![self.initial];
        all.extend(self.changes.iter().map(|(_, offset)| *offset));
        for offset in all {
            let secs = offset.local_minus_utc();
            let instant = local.and_utc().timestamp() - i64::from(secs);
            if self.offset_at(instant).local_minus_utc() == secs && !out.contains(&offset) {
                out.push(offset);
            }
        }
        // Larger offset first: it yields the earlier instant, which is what
        // `LocalResult::Ambiguous` wants in its first slot.
        out.sort_unstable_by_key(|o| std::cmp::Reverse(o.local_minus_utc()));
        out
    }
}

impl TimeZone for TestZone {
    type Offset = FixedOffset;

    fn from_offset(offset: &FixedOffset) -> Self {
        Self {
            initial: offset.fix(),
            changes: Vec::new(),
        }
    }

    fn offset_from_local_date(&self, local: &NaiveDate) -> LocalResult<FixedOffset> {
        match local.and_hms_opt(0, 0, 0) {
            Some(at) => self.offset_from_local_datetime(&at),
            None => LocalResult::None,
        }
    }

    fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<FixedOffset> {
        match self.candidates(local).as_slice() {
            [] => LocalResult::None,
            [only] => LocalResult::Single(*only),
            [earlier, later, ..] => LocalResult::Ambiguous(*earlier, *later),
        }
    }

    fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
        self.offset_at(utc.and_time(chrono::NaiveTime::MIN).and_utc().timestamp())
    }

    fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
        self.offset_at(utc.and_utc().timestamp())
    }
}
