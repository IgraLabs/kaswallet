use kaspa_addresses::Address;
use kaspa_grpc_client::GrpcClient;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_rpc_core::RpcRawBlock;
use std::sync::Arc;

pub async fn mine_block(kaspad_client: Arc<GrpcClient>, address: &str) -> RpcRawBlock {
    let address: Address = address
        .try_into()
        .unwrap_or_else(|_| panic!("Invalid address: {}", address));

    let block_template = kaspad_client
        .get_block_template(address, vec![])
        .await
        .expect("Error getting block template");

    kaspad_client
        .submit_block(block_template.block.clone(), false)
        .await
        .expect("Error submitting block");

    block_template.block
}

pub async fn mine_n_blocks(
    kaspad_client: Arc<GrpcClient>,
    address: &str,
    n: u32,
) -> Vec<RpcRawBlock> {
    let mut blocks = Vec::new();
    for _ in 0..n {
        let block = mine_block(kaspad_client.clone(), address).await;
        blocks.push(block);
    }
    blocks
}
