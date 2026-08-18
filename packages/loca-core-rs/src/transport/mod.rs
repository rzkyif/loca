//! HTTPS/WSS transport: the router every package mounts into, the per-boot TLS,
//! the WebSocket hub, the frame bus, and the core-status aggregator.
//!
//! Work package **F6** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here, except that
//! [`router`] is orchestrator-owned because it is where every other package's
//! route slice is mounted.
//!
//! # Two frontends, one service
//!
//! ```text
//! desktop                                mobile
//!   TcpListener                            Tauri command core_http
//!     tokio-rustls TlsAcceptor               |
//!       hyper auto::Builder                  |
//!         +--------------------------------- +
//!         |
//!         v
//!     axum::Router          the seam: a tower::Service
//!         |
//!     route slices, middleware, handlers, frame bus
//! ```
//!
//! [`Core`] is the whole service, reachable with or without a socket.
//! [`server`] is the only file below it that knows what a socket is, and
//! [`Core::call`] reaches the same router with none. That is not a convenience:
//! it is why the mobile client can embed the real Core rather than a
//! reimplementation, and why the two paths cannot drift.

pub mod frame_bus;
pub mod health;
pub mod http;
pub mod inproc;
pub mod middleware;
pub mod router;
pub mod server;
pub mod state;
pub mod status;
pub mod tls;
pub mod ws;

#[cfg(any(test, feature = "in-memory"))]
pub mod testing;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;
use tokio::sync::watch;

use crate::error::Result;

pub use frame_bus::{Connection, ConnectionSeed, FrameBus, InboundHandler, Outbound};
pub use http::{bytes, no_content, Json};
pub use inproc::{InProcRequest, InProcResponse};
pub use middleware::{AuthedClient, PeerAddr};
pub use state::{AppContext, ClientAuth, ClientIdentity, StatusProvider};
pub use status::{CoreStatusService, SidecarSource};
pub use tls::TlsMaterial;
pub use ws::{handle_client_frame, Handled};

/// The running service.
///
/// Constructed once by the frontend. On desktop it is then handed to
/// [`Core::serve`]; on mobile it is called directly through [`Core::call`] and
/// [`Core::connect`].
pub struct Core {
    ctx: AppContext,
    router: Router,
    shutdown: watch::Sender<bool>,
}

impl Core {
    /// `services` carries the handles a route slice needs by value, including
    /// the concrete `Auth` alongside the `dyn ClientAuth` already in `ctx`.
    /// `boot` owns every one of them; see [`router::Services`] for why they are
    /// threaded rather than constructed per slice.
    pub fn new(ctx: AppContext, services: router::Services) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            router: router::build(ctx.clone(), services),
            ctx,
            shutdown,
        }
    }

    pub fn context(&self) -> &AppContext {
        &self.ctx
    }

    /// The assembled router. Cloning is cheap and every clone routes
    /// identically, which is what both frontends rely on.
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    pub fn frames(&self) -> &Arc<FrameBus> {
        &self.ctx.frames
    }

    /// Attach a client connection with no socket underneath it.
    ///
    /// The mobile frontend pumps the returned connection into Tauri events; the
    /// WebSocket route pumps an identical one into a socket. Neither the bus
    /// nor any service that broadcasts through it can tell them apart.
    pub fn connect(&self, client_id: &str) -> Connection {
        self.ctx.frames.register(client_id)
    }

    /// Bind and serve until [`Core::shutdown`] is called. Desktop only.
    pub async fn serve(&self, tls: &TlsMaterial, host: &str, port: u16) -> Result<()> {
        let listener = server::bind(tls, host, port).await?;
        listener
            .serve(self.router(), self.shutdown.subscribe())
            .await;
        Ok(())
    }

    /// Bind without serving, for a caller that needs the resolved port first.
    pub async fn bind(&self, tls: &TlsMaterial, host: &str, port: u16) -> Result<server::Listener> {
        server::bind(tls, host, port).await
    }

    /// Serve an already-bound listener.
    pub async fn serve_bound(&self, listener: server::Listener) {
        listener
            .serve(self.router(), self.shutdown.subscribe())
            .await;
    }

    /// Stop accepting, drain in-flight connections, and close every client
    /// connection.
    pub fn shutdown(&self) {
        self.ctx.frames.shutdown();
        let _ = self.shutdown.send(true);
    }
}
