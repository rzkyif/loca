//! Choosing a transport for a server. Separate from the manager so a test can
//! substitute a connector that returns an in-process session.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::Result;
use crate::paths::Paths;
use crate::secrets::Vault;
use crate::sidecars::Manager as Sidecars;

use super::registry::ServerConfig;
use super::session::{Connected, Connector};
use super::stdio::StdioSupport;
use crate::contract::api::McpTransportKind;

/// The production connector.
pub struct LiveConnector {
    paths: Arc<Paths>,
    vault: Vault,
    sidecars: Arc<Sidecars>,
    stdio: StdioSupport,
}

impl LiveConnector {
    pub fn new(
        paths: Arc<Paths>,
        vault: Vault,
        sidecars: Arc<Sidecars>,
        stdio: StdioSupport,
    ) -> Self {
        Self {
            paths,
            vault,
            sidecars,
            stdio,
        }
    }
}

#[async_trait]
impl Connector for LiveConnector {
    async fn connect(&self, server: &ServerConfig) -> Result<Connected> {
        match server.kind {
            McpTransportKind::Stdio => {
                super::stdio::connect(server, &self.paths, &self.sidecars, self.stdio).await
            }
            McpTransportKind::Remote => super::remote::connect(server, &self.vault).await,
        }
    }

    fn stdio_supported(&self) -> bool {
        self.stdio.is_available()
    }
}
