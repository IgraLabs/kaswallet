use kaspa_consensus_core::network::NetworkId;
use kaspa_wrpc_client::{KaspaRpcClient, WrpcEncoding};
use log::{error, info};
use std::error::Error;
use std::sync::Arc;
use workflow_websocket::client::{ConnectOptions, ConnectStrategy};

pub async fn connect(
    server: &Option<String>,
    network_id: &NetworkId,
) -> Result<Arc<KaspaRpcClient>, Box<dyn Error + Send + Sync>> {
    let url = match server {
        Some(server) => server,
        None => &format!(
            "ws://localhost:{}",
            network_id.network_type.default_borsh_rpc_port()
        ),
    };

    info!("Connecting to kaspa node at {}", url);

    let options = Some(ConnectOptions {
        block_async_connect: true,
        strategy: ConnectStrategy::Fallback,
        url: Some(url.clone()),
        ..Default::default()
    });

    let rpc_client = Arc::new(KaspaRpcClient::new_with_args(
        WrpcEncoding::Borsh,
        Some(url),
        None,
        Some(*network_id),
        None,
    )?);

    rpc_client.connect(options).await.map_err(|e| {
        let error_message = format!("Failed to connect to kaspa node: {}", e);
        error!("{}", error_message);
        e
    })?;
    info!("Connected to kaspa node successfully");

    Ok(rpc_client)
}
