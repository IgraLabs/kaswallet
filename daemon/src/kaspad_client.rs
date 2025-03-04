use std::error::Error;
use std::sync::Arc;
use kaspa_consensus_core::network::NetworkId;
use kaspa_wrpc_client::{KaspaRpcClient, WrpcEncoding};
use workflow_websocket::client::{ConnectOptions, ConnectStrategy};
use crate::args::Args;

pub fn connect(server: Option<String>, network_id: NetworkId) -> Result<Arc<KaspaRpcClient>, dyn Error>{
    let mut url = server.unwrap_or_else(|| "localhost".to_string());
    if !url.contains(":"){
        url.push_str(&format!(":{}",
                              network_id.network_type.default_borsh_rpc_port()))
    }

    let options = ConnectOptions {
        block_async_connect: true,
        strategy: ConnectStrategy::Fallback,
        url: Some(url.clone()),
        ..Default::default()
    };

    let rpc_client = Arc::new(
        KaspaRpcClient::new_with_args(
            WrpcEncoding::Borsh,
            Some(&format!("wrpc://{}", url)),
            None,
            Some(network_id),
            None)?);

    Ok(rpc_client)
}