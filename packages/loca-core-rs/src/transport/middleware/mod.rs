//! The layers a route slice applies, and the extractors its handlers read.
//!
//! Re-exported flat so a slice needs one import line:
//!
//! ```ignore
//! use crate::transport::middleware::{from_fn_with_state, require_client, AuthedClient};
//! ```

pub mod auth;
pub mod body_limit;
pub mod cors;

pub use auth::{require_admin_token, require_client, AuthedClient, PeerAddr, LOCAL_PEER};
pub use body_limit::MAX_BODY_BYTES;
pub use cors::is_origin_allowed;

/// Re-exported so a slice applying an auth layer imports it from here rather
/// than reaching into axum for that one line.
pub use axum::middleware::{from_fn, from_fn_with_state};
