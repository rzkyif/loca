//! The [`ClientAuth`] seam: what the transport calls to turn a bearer into an
//! identity, spend a WebSocket ticket, and check the admin token.
//!
//! The transport owns the layers and extractors that apply this
//! (`transport/middleware/auth.rs`); this package owns what they mean, which is
//! the parity surface. Splitting "what a valid bearer is" from "where it is
//! required" across two packages is how the two drift.
//!
//! # The blocking question, which is a real one
//!
//! [`ClientAuth`] is synchronous, so [`Auth::authenticate`] and
//! [`Auth::verify_admin_token`], which are async and reach the database and the
//! filesystem, are driven here through their blocking counterparts. That is what
//! the trait's own contract asks for: every operation behind it is a point query
//! or a map lookup.
//!
//! It is still a tokio worker thread parked on `Db`'s connection mutex for the
//! duration of that query, which is exactly what `Db::call` exists to avoid, and
//! the two guidances point in opposite directions. The exposure is bounded: the
//! statements are `prepare_cached` point queries against a local file, and the
//! contention window is one other query. If the trait ever becomes async, every
//! method here collapses to a direct `.await` on the inherent one and this note
//! goes with it.

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
            // The CSPRNG failed, which means the OS could not supply entropy.
            // The trait has nowhere to put an error, so this returns a value
            // that was never inserted into the table: presenting it on the
            // upgrade is rejected like any unknown ticket, and the client
            // retries. Falling back to a predictable value instead would hand
            // out a working credential.
            Err(err) => {
                tracing::error!(target: "loca::auth", "could not mint a WS ticket: {}", err.message);
                String::new()
            }
        }
    }

    fn consume_ws_ticket(&self, ticket: &str) -> Option<String> {
        // An empty ticket is never in the table, but rejecting it here means a
        // request with no ticket at all cannot collide with the failure above.
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
/// transport's identity. `pub(crate)` rather than private because `boot` needs
/// it too, for the embedded frontend's local client, and a second copy of a
/// four-field mapping is exactly the shape D-049 warns about: cheap to duplicate
/// and therefore certain to drift.
pub(crate) fn to_identity(client: AuthedClient) -> ClientIdentity {
    ClientIdentity {
        id: client.id,
        name: client.name,
        created_at_ms: client.created_at_ms,
        last_seen_ms: client.last_seen_ms,
    }
}

/// Drive one of this module's async methods to completion from a synchronous
/// caller.
///
/// On the multi-threaded runtime, which is what the server runs,
/// `block_in_place` hands the worker's other tasks to a sibling thread first, so
/// parking here does not stall them. A bare `Handle::block_on` would.
///
/// Anywhere else, the future goes to a borrowed thread with its own runtime.
/// That covers both a caller with no runtime at all and a caller inside a
/// current-thread one, where blocking in place is not merely slow but a panic:
/// tokio refuses to start a runtime from inside an async context, and there is
/// no worker to hand the work to. It costs a thread spawn, and it is reached
/// only by a single-threaded frontend or a `#[tokio::test]` left at its default
/// flavour, never by the server.
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
                    // Reachable only when the process cannot allocate a thread
                    // or an event loop. It becomes a failed request, which every
                    // caller can already express.
                    .map_err(|err| internal(format!("could not build a runtime: {err}")))?;
                Ok(runtime.block_on(future))
            })
            .join();
        match joined {
            Ok(result) => result,
            // A panic inside the future is re-raised on this thread, so the hop
            // is invisible: it behaves exactly as it would have without it.
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}
