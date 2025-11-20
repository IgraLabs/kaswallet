use kaspa_consensus_core::network::NetworkId;
use kaspa_grpc_client::GrpcClient;
use kaspa_wallet_core::rpc::NotificationMode;
use log::{error, info};
use std::error::Error;
use std::sync::Arc;

pub async fn connect(
    server: &Option<String>,
    network_id: &NetworkId,
) -> Result<Arc<GrpcClient>, Box<dyn Error + Send + Sync>> {
    let url = match server {
        Some(server) => server,
        None => &format!(
            "grpc://localhost:{}",
            network_id.network_type.default_rpc_port()),
    };
    info!("Connecting to kaspa node at {}", url);

    let client = Arc::new(GrpcClient::connect(url.to_string()).await?);

    info!("Connected to kaspa node successfully");

    Ok(client)
}
