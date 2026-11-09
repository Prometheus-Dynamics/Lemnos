//! The Orion side. One provider session on the async runtime. It checks the node, registers
//! the provider, and then sends what the mirror says has changed.

use std::sync::Arc;
use std::time::Duration;

use orion_client::{ClientError, LocalNodeRuntime, LocalProviderApp};
use orion_control_plane::ProviderRecord;
use orion_core::{NodeId, ProviderId, ResourceType};

use crate::backoff::Backoff;
use crate::config::Config;
use crate::mirror::{Changes, REFRESH};
use crate::{CLIENT_NAME, PROVIDER_ID, RESOURCE_TYPE, Shared, log};

/// Publishes what the mirror holds, for as long as the process runs.
///
/// Waits for the mirror to change or for the refresh tick. A publish that fails drops the session,
/// and the next connect publishes everything again. Orion's node is checked on every connect,
/// and nothing is published to a node that is not `ORION_NODE_ID`.
pub async fn run(config: Config, shared: Arc<Shared>) {
    let runtime = LocalNodeRuntime::new(config.orion_socket.clone(), config.orion_stream.clone());
    let mut backoff = Backoff::new();
    let mut refusals = 0u32;
    let mut refresh = tokio::time::interval(REFRESH);
    let mut session: Option<LocalProviderApp> = None;
    loop {
        if session.is_none() {
            match connect(&runtime, &config.node_id).await {
                Ok(app) => {
                    log(&format!("connected to Orion node {}", config.node_id));
                    backoff.reset();
                    refusals = 0;
                    shared.with(|mirror| mirror.mark_all());
                    session = Some(app);
                }
                Err(message) => {
                    log(&message);
                    tokio::time::sleep(backoff.next_delay()).await;
                    continue;
                }
            }
        }
        tokio::select! {
            () = shared.wake.notified() => {}
            _ = refresh.tick() => shared.with(|mirror| mirror.mark_all()),
        }
        let changes = shared.with(|mirror| mirror.take());
        if changes.is_empty() {
            continue;
        }
        let Some(app) = session.as_ref() else {
            continue;
        };
        match publish(app, changes).await {
            Ok(()) => {
                backoff.reset();
                refusals = 0;
                // Changes that arrive during this gap merge in the mirror.
                tokio::time::sleep(MIN_GAP).await;
            }
            Err(error) => {
                // Orion answered and refused: its local rate limit, or a request it will not take.
                // The session is fine, so the changes are sent again in full after a wait. Three
                // refusals in a row mean the session is no longer welcome, and it is made again.
                // Any other error means the connection is gone.
                let refused = matches!(error, ClientError::Rejected(_));
                refusals = if refused { refusals + 1 } else { 0 };
                if !refused || refusals >= MAX_REFUSALS {
                    log(&format!("Orion publish failed, reconnecting: {error}"));
                    session = None;
                    refusals = 0;
                } else {
                    log(&format!("Orion refused a publish, retrying: {error}"));
                }
                shared.with(|mirror| mirror.mark_all());
                tokio::time::sleep(backoff.next_delay()).await;
            }
        }
    }
}

/// The shortest time between two publishes. Orion refuses more than 256 messages a second from
/// one client, so a fast sensor would otherwise spend its time being refused.
const MIN_GAP: Duration = Duration::from_millis(5);

/// Refusals in a row before the session is made again.
const MAX_REFUSALS: u32 = 3;

/// Sends the resource list when it changed, then the statuses.
async fn publish(app: &LocalProviderApp, changes: Changes) -> Result<(), ClientError> {
    if let Some(records) = changes.records {
        app.publish_resources(records).await?;
    }
    if !changes.status.is_empty() {
        app.publish_status(changes.status).await?;
    }
    Ok(())
}

/// Opens a session on the node and registers the provider there. It works only when the node
/// Orion welcomes this session from is the one named by `ORION_NODE_ID`.
async fn connect(runtime: &LocalNodeRuntime, expected: &str) -> Result<LocalProviderApp, String> {
    let stream = runtime
        .control_stream(CLIENT_NAME)
        .await
        .map_err(|error| format!("Orion not available: {error}"))?;
    let welcomed = stream.node_id().as_str().to_owned();
    drop(stream);
    check_node(&welcomed, expected)?;
    let provider = ProviderRecord::builder(
        ProviderId::new(PROVIDER_ID),
        NodeId::new(expected.to_owned()),
    )
    .resource_type(ResourceType::new(RESOURCE_TYPE))
    .build();
    let app = runtime
        .provider_session(CLIENT_NAME, provider)
        .map_err(|error| format!("Orion provider: {error}"))?;
    app.register()
        .await
        .map_err(|error| format!("Orion register: {error}"))?;
    Ok(app)
}

/// Orion refuses another node's records, so a welcome from any other node stops publishing.
pub fn check_node(welcomed: &str, expected: &str) -> Result<(), String> {
    if welcomed == expected {
        Ok(())
    } else {
        Err(format!(
            "Orion's node is {welcomed:?} but ORION_NODE_ID is {expected:?}; not publishing"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_welcomed_node_must_be_the_configured_one() {
        assert!(check_node("node-a", "node-a").is_ok());
        let refused = check_node("node-b", "node-a").unwrap_err();
        assert!(refused.contains("node-b"));
        assert!(refused.contains("not publishing"));
    }
}
