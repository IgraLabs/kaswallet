use crate::daemon::DaemonStartError;
use kaspa_consensus_core::network::NetworkId;
use kaspa_grpc_client::GrpcClient;
use log::info;

pub async fn connect(
    server: &Option<String>,
    network_id: &NetworkId,
) -> Result<GrpcClient, DaemonStartError> {
    let url = match server {
        Some(server) => server,
        None => &format!(
            "grpc://localhost:{}",
            network_id.network_type.default_rpc_port()
        ),
    };
    info!("Connecting to kaspa node at {}", url);

    let client = GrpcClient::connect(url.to_string())
        .await
        .map_err(|e| DaemonStartError::FailedToConnectToKaspad(url.to_string(), e))?;

    info!("Connected to kaspa node successfully");

    Ok(client)
}
