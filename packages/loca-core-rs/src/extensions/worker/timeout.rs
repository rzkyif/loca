//! A tool call's time budget: a PAUSABLE countdown, not a deadline.
//!
//! While a call waits on a human the budget is paused, so a slow answer never
//! counts against the tool's COMPUTE time. A fixed deadline gets that exactly
//! backwards: a prompt left open longer than the remaining budget kills its own
//! call while the form is still on screen.
//!
//! So this type owns no timer and spawns no task. It answers one question,
//! [`CallTimeout::deadline`], and the call's driver sleeps until whatever it
//! currently says. `arm` after a `pause` therefore acts as "resume".
//!
//! Every instant is a [`tokio::time::Instant`]: a `std` deadline never expires
//! under a paused runtime, so a test built on one passes whatever the code
//! does.

use std::time::Duration;

use tokio::time::Instant;

/// A call's remaining compute budget, and whether it is currently counting
/// down.
#[derive(Debug)]
pub struct CallTimeout {
    /// What is left. Reduced by [`CallTimeout::pause`], never by the passage of
    /// time on its own.
    budget: Duration,
    /// When the current countdown started. `None` while paused or disarmed.
    armed_at: Option<Instant>,
}

impl CallTimeout {
    pub fn new(budget: Duration) -> Self {
        Self {
            budget,
            armed_at: None,
        }
    }

    /// True while the budget is counting down.
    pub fn armed(&self) -> bool {
        self.armed_at.is_some()
    }

    /// What is left of the budget, whether or not it is armed.
    #[cfg(test)]
    pub fn remaining(&self) -> Duration {
        self.budget
    }

    /// Start, or resume, the countdown over the remaining budget. A spent
    /// budget is a no-op, and so is arming something already armed: the EARLIER
    /// deadline wins, or a call that keeps arming itself runs forever.
    pub fn arm(&mut self) {
        if self.budget.is_zero() || self.armed_at.is_some() {
            return;
        }
        self.armed_at = Some(Instant::now());
    }

    /// When the budget runs out, or `None` while it is not counting down. The
    /// caller sleeps until this and calls [`CallTimeout::expire`] if it
    /// arrives; recomputing it on every `arm` is what makes it pausable.
    pub fn deadline(&self) -> Option<Instant> {
        self.armed_at.map(|at| at + self.budget)
    }

    /// Stop the countdown, subtracting what it consumed. Idempotent, because
    /// the anchor is cleared here: two prompts opening back to back must cost
    /// the budget one span of wall clock, not two.
    pub fn pause(&mut self) {
        let Some(armed_at) = self.armed_at.take() else {
            return;
        };
        self.budget = self.budget.saturating_sub(armed_at.elapsed());
    }

    /// Cancel the countdown for good. The budget is not preserved, because
    /// every caller of this is a terminal settle.
    pub fn disarm(&mut self) {
        self.armed_at = None;
    }

    /// The deadline arrived. Clears the anchor BEFORE the owner acts, so the
    /// call's defensive re-arm from inside its own expiry handling is not a
    /// silent no-op.
    pub fn expire(&mut self) {
        self.armed_at = None;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_secs(60);

    /// Literal seconds throughout rather than arithmetic on `BUDGET`, which
    /// would move with it.
    fn budget() -> CallTimeout {
        CallTimeout::new(BUDGET)
    }

    async fn advance(secs: u64) {
        tokio::time::advance(Duration::from_secs(secs)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_human_answer_does_not_burn_the_tools_compute_budget() {
        let mut t = budget();
        t.arm();
        advance(10).await; // 10s of compute
        t.pause();
        advance(3_600).await; // an hour on a prompt
        t.arm();

        // 50s left, measured from the resume, not 60s and not -3550s.
        let left = t.deadline().unwrap() - Instant::now();
        assert_eq!(left, Duration::from_secs(50));
    }

    #[tokio::test(start_paused = true)]
    async fn resuming_does_not_hand_the_call_a_fresh_full_budget() {
        // A `pause` that forgets to subtract looks correct until the resumed
        // window is checked for being SHORTER.
        let mut t = budget();
        t.arm();
        advance(45).await;
        t.pause();
        assert_eq!(t.remaining(), Duration::from_secs(15));
        t.arm();
        assert_eq!(
            t.deadline().unwrap() - Instant::now(),
            Duration::from_secs(15)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn two_prompts_back_to_back_cost_the_budget_one_span_not_two() {
        // Without the cleared anchor the same 20 seconds is charged twice.
        let mut t = budget();
        t.arm();
        advance(20).await;
        t.pause();
        t.pause();
        t.pause();
        assert_eq!(t.remaining(), Duration::from_secs(40));
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_only_shrinks_while_it_is_armed() {
        let mut t = budget();
        advance(3_600).await; // an hour before the call ever started
        t.pause();
        assert_eq!(t.remaining(), Duration::from_secs(60));
        t.arm();
        assert_eq!(
            t.deadline().unwrap() - Instant::now(),
            Duration::from_secs(60)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn arming_an_already_armed_budget_keeps_the_earlier_deadline() {
        // A re-anchor would let a call that keeps arming itself run forever.
        let mut t = budget();
        t.arm();
        let first = t.deadline().unwrap();
        advance(30).await;
        t.arm();
        assert_eq!(t.deadline().unwrap(), first);
        assert_eq!(
            t.deadline().unwrap() - Instant::now(),
            Duration::from_secs(30)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_spent_budget_never_arms_again() {
        let mut t = budget();
        t.arm();
        advance(60).await;
        t.pause();
        assert_eq!(t.remaining(), Duration::ZERO);
        t.arm();
        assert!(!t.armed(), "a spent budget armed");
        assert!(t.deadline().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_clears_the_anchor_so_the_owner_can_re_arm_from_inside_it() {
        // With the anchor left set, the owner's defensive re-arm is a silent
        // no-op and the call hangs with no timer at all.
        let mut t = budget();
        t.arm();
        advance(60).await;
        assert!(Instant::now() >= t.deadline().unwrap());
        t.expire();
        assert!(!t.armed());
        t.arm();
        assert!(t.armed(), "the owner could not re-arm from inside expiry");
    }

    #[tokio::test(start_paused = true)]
    async fn disarm_stops_the_countdown_for_good() {
        let mut t = budget();
        t.arm();
        advance(10).await;
        t.disarm();
        assert!(!t.armed());
        assert!(t.deadline().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn the_shipping_call_budget_is_sixty_seconds() {
        // Pinned against the pool's default: a divergence is invisible until a
        // call outlives its worker.
        assert_eq!(
            super::super::pool::DEFAULT_POOL_CONFIG.call_timeout,
            Duration::from_secs(60)
        );
        let mut t = CallTimeout::new(super::super::pool::DEFAULT_POOL_CONFIG.call_timeout);
        t.arm();
        assert_eq!(
            t.deadline().unwrap() - Instant::now(),
            Duration::from_secs(60)
        );
    }
}
