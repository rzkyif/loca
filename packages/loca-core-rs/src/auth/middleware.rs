//! The [`ClientAuth`] seam: what the transport calls to turn a bearer into an
//! identity, spend a WebSocket ticket, and check the admin token.
//!
//! The transport owns the layers and extractors that apply this
//! (`transport/middleware/auth.rs`); this package owns what they mean.
//!
//! [`ClientAuth`] is synchronous, so the two async inherent methods are driven
//! here through [`block_on`]. That parks a worker thread on `Db`'s connection
//! mutex, which is bounded: the statements are `prepare_cached` point queries
//! against a local file. If the trait ever becomes async, every method here
//! collapses to a direct `.await`.

use crate::error::{internal, Result};
use crate::transport::state::{ClientAuth, ClientIdentity};

use super::{Auth, AuthedClient};

impl ClientAuth for Auth {
    fn authenticate(&self, bearer: Option<&str>) -> Result<ClientIdentity> {
        block_on(self.authenticate(bearer))?.map(to_identity)
    }

    fn mint_ws_ticket(&self, client_id: &str) -> String {
        match Auth::mint_ws_ticket(self, client_id) {
            Ok(ticket) => ticket.into_inner(),
            // The trait has nowhere to put an error, so this returns a value
            // that was never inserted: presenting it is rejected like any
            // unknown ticket. A predictable fallback would be a credential.
            Err(err) => {
                tracing::error!(target: "loca::auth", "could not mint a WS ticket: {}", err.message);
                String::new()
            }
        }
    }

    fn consume_ws_ticket(&self, ticket: &str) -> Option<String> {
        // Rejected here so a request with no ticket cannot collide with the
        // empty string the mint failure above returns.
        if ticket.is_empty() {
            return None;
        }
        Auth::consume_ws_ticket(self, ticket)
    }

    fn verify_admin_token(&self, provided: Option<&str>) -> Result<()> {
        block_on(self.verify_admin_token(provided))?
    }

    fn has_admin_password(&self) -> bool {
        Auth::has_admin_password(self)
    }
}

/// The one conversion between the pairing package's client row and the
/// transport's identity. `pub(crate)` because `boot` needs it too, for the
/// embedded frontend's local client.
pub(crate) fn to_identity(client: AuthedClient) -> ClientIdentity {
    ClientIdentity {
        id: client.id,
        name: client.name,
        created_at_ms: client.created_at_ms,
        last_seen_ms: client.last_seen_ms,
    }
}

/// Drive one of this module's async methods from a synchronous caller.
///
/// On the multi-threaded runtime the server runs, `block_in_place` hands the
/// worker's other tasks to a sibling thread first, where a bare
/// `Handle::block_on` would stall them. Anywhere else the future goes to a
/// borrowed thread with its own runtime, which covers a caller with no runtime
/// and a current-thread one, where blocking in place would panic.
fn block_on<F>(future: F) -> Result<F::Output>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            Ok(tokio::task::block_in_place(|| handle.block_on(future)))
        }
        _ => block_on_borrowed_thread(future),
    }
}

fn block_on_borrowed_thread<F>(future: F) -> Result<F::Output>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        let joined = scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    // Only when the process cannot allocate a thread or an
                    // event loop, which becomes a failed request.
                    .map_err(|err| internal(format!("could not build a runtime: {err}")))?;
                Ok(runtime.block_on(future))
            })
            .join();
        match joined {
            Ok(result) => result,
            // Re-raised here, so the thread hop is invisible.
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}
