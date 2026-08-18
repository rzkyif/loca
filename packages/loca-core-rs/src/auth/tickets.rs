//! Short-lived, single-use WebSocket upgrade tickets.
//!
//! The property this exists to preserve: the long-lived bearer stays in the
//! `Authorization` header of the mint call, and only a throwaway ticket travels
//! in the `ws://` query string. A fronting reverse proxy's access log therefore
//! captures at most a spent 30-second token, never a full-privilege bearer.

use indexmap::IndexMap;

use super::{MAX_WS_TICKETS, WS_TICKET_TTL_MS};

struct Ticket {
    client_id: String,
    expires_at_ms: i64,
}

/// Live tickets, in mint order.
///
/// Ordered because over-cap eviction drops the oldest, and a `HashMap` would
/// drop an arbitrary one. Removal is always `shift_remove`: `swap_remove` is
/// O(1) but moves the last entry into the hole, which silently destroys the
/// ordering the eviction rule depends on.
pub(crate) struct TicketTable {
    tickets: IndexMap<String, Ticket>,
}

impl TicketTable {
    pub(crate) fn new() -> Self {
        Self {
            tickets: IndexMap::new(),
        }
    }

    pub(crate) fn insert(&mut self, ticket: String, client_id: &str, now_ms: i64) {
        self.prune(now_ms);
        self.tickets.insert(
            ticket,
            Ticket {
                client_id: client_id.to_owned(),
                expires_at_ms: now_ms + WS_TICKET_TTL_MS,
            },
        );
    }

    /// Validate and consume a ticket, returning the owning client id.
    ///
    /// The ticket is removed whether or not it was still valid, so a replay and
    /// a presentation past the TTL are both rejected and neither leaves anything
    /// behind to retry against.
    pub(crate) fn consume(&mut self, ticket: &str, now_ms: i64) -> Option<String> {
        let entry = self.tickets.shift_remove(ticket)?;
        if now_ms > entry.expires_at_ms {
            return None;
        }
        Some(entry.client_id)
    }

    /// Drop expired tickets, then bound the table, so a client that mints and
    /// never connects cannot pin memory.
    fn prune(&mut self, now_ms: i64) {
        self.tickets.retain(|_, t| now_ms <= t.expires_at_ms);
        while self.tickets.len() >= MAX_WS_TICKETS {
            if self.tickets.shift_remove_index(0).is_none() {
                break;
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.tickets.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_valid_exactly_once() {
        let mut t = TicketTable::new();
        t.insert("tkt".into(), "client-42", 0);
        assert_eq!(t.consume("tkt", 0).as_deref(), Some("client-42"));
        assert_eq!(t.consume("tkt", 0), None);
    }

    #[test]
    fn an_unknown_ticket_is_rejected() {
        let mut t = TicketTable::new();
        assert_eq!(t.consume("never-minted", 0), None);
    }

    #[test]
    fn the_last_valid_millisecond_still_works_and_the_next_does_not() {
        let mut t = TicketTable::new();
        t.insert("a".into(), "c", 0);
        assert!(t.consume("a", WS_TICKET_TTL_MS).is_some());
        t.insert("b".into(), "c", 0);
        assert!(t.consume("b", WS_TICKET_TTL_MS + 1).is_none());
    }

    #[test]
    fn an_expired_ticket_is_consumed_even_though_it_is_rejected() {
        let mut t = TicketTable::new();
        t.insert("a".into(), "c", 0);
        assert_eq!(t.consume("a", WS_TICKET_TTL_MS + 1), None);
        assert_eq!(t.len(), 0, "an expired presentation still removes it");
    }

    #[test]
    fn expired_tickets_are_swept_on_the_next_mint() {
        let mut t = TicketTable::new();
        for i in 0..10 {
            t.insert(format!("old{i}"), "c", 0);
        }
        t.insert("fresh".into(), "c", WS_TICKET_TTL_MS + 1);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_table_is_bounded_and_evicts_the_oldest() {
        let mut t = TicketTable::new();
        for i in 0..MAX_WS_TICKETS {
            t.insert(format!("t{i}"), "c", 0);
        }
        assert_eq!(t.len(), MAX_WS_TICKETS);
        t.insert("overflow".into(), "c", 0);
        assert_eq!(t.len(), MAX_WS_TICKETS, "steady state is exactly the cap");
        assert_eq!(t.consume("t0", 0), None, "the oldest was evicted");
        assert!(t.consume("t1", 0).is_some(), "the next oldest survived");
        assert!(t.consume("overflow", 0).is_some());
    }
}
