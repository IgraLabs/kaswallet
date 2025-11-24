use kaspa_grpc_client::GrpcClient;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_rpc_core::RpcBlock;
use std::error::Request;

pub fn mine_block(kaspad_client: GrpcClient) -> RpcBlock {
    let block_template = kaspad_client.get_block_template(Request::new(GetBl)).await;
}
