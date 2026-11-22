//! Bounded, ephemeral wake hints. Canonical run receipts remain authoritative.
use crate::MemoryServer;
use rmcp::service::{Peer, RoleServer};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

const MAX_PEERS: usize = 128;
const QUEUE_CAPACITY: usize = 32;
const SEND_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(crate) struct Registry {
    peers: Mutex<HashMap<String, mpsc::Sender<String>>>,
}

impl Registry {
    pub(crate) fn register(self: &Arc<Self>, server: &MemoryServer, peer: Peer<RoleServer>) {
        let Ok(owner) = crate::mcp_tasks::notification_owner(server) else {
            return;
        };
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, mut receiver) = mpsc::channel::<String>(QUEUE_CAPACITY);
        {
            let mut peers = self.peers.lock().unwrap_or_else(|p| p.into_inner());
            peers.retain(|_, sender| !sender.is_closed());
            if peers.len() >= MAX_PEERS {
                return;
            }
            peers.insert(id.clone(), sender);
        }
        let registry = Arc::downgrade(self);
        let server = server.clone();
        tokio::spawn(async move {
            let mut expiry = tokio::time::interval(Duration::from_secs(1));
            loop {
                let dispatch = tokio::select! {
                    _ = expiry.tick() => {
                        if peer.is_transport_closed() { break; }
                        continue;
                    }
                    dispatch = receiver.recv() => match dispatch {
                        Some(dispatch) => dispatch,
                        None => break,
                    },
                };
                // Recheck current admission, scope, physical store and profile
                // before opening the receipt. Registration grants no lasting access.
                if crate::mcp_tasks::notification_owner(&server).ok().as_ref() != Some(&owner) {
                    break;
                }
                let Ok(Some(notification)) =
                    crate::mcp_tasks::status_notification(&server, &dispatch)
                else {
                    continue;
                };
                if !matches!(
                    tokio::time::timeout(SEND_TIMEOUT, peer.send_notification(notification)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            if let Some(registry) = registry.upgrade() {
                registry
                    .peers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
            }
        });
    }

    /// Call only after committing status and releasing its mutex/fence. There
    /// is no IO or await on the writer; a full peer queue drops that peer.
    pub(crate) fn notify(&self, dispatch: &str) {
        let mut peers = self.peers.lock().unwrap_or_else(|p| p.into_inner());
        peers.retain(|_, sender| sender.try_send(dispatch.to_string()).is_ok());
    }

    #[cfg(test)]
    pub(crate) fn peer_count(&self) -> usize {
        self.peers.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// Covers all early returns after a run has been admitted, as well as final
/// background completion. Created outside status-lock scopes.
pub(crate) struct NotifyOnDrop<'a> {
    server: &'a MemoryServer,
    dispatch: Option<String>,
}

impl<'a> NotifyOnDrop<'a> {
    pub(crate) fn new(server: &'a MemoryServer, dispatch: Option<&str>) -> Self {
        Self {
            server,
            dispatch: dispatch.map(str::to_owned),
        }
    }
}

impl Drop for NotifyOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(dispatch) = &self.dispatch {
            self.server.task_notifications.notify(dispatch);
        }
    }
}
