use common::error_location::ErrorLocation;
use common::errors::{RpcError, WalletError, WalletResult};
use kaspa_consensus_core::network::NetworkId;
use kaspa_grpc_client::GrpcClient;
use tracing::info;

pub async fn connect(server: &Option<String>, network_id: &NetworkId) -> WalletResult<GrpcClient> {
    let url = match server {
        Some(server) => server.clone(),
        None => format!(
            "grpc://localhost:{}",
            network_id.network_type.default_rpc_port()
        ),
    };
    info!("Connecting to kaspa node at {}", url);

    let client = GrpcClient::connect(url.clone()).await.map_err(|e| {
        WalletError::from(RpcError::Connect {
            endpoint: url.clone(),
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })
    })?;

    info!("Connected to kaspa node successfully");

    Ok(client)
}
