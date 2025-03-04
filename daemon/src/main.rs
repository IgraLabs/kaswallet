use clap::Parser;
use ::log::info;
use std::error::Error;
use tonic::transport::Server;
use wallet_proto::wallet_proto::wallet_server::WalletServer;

mod args;
mod log;
mod service;
mod kaspad_client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = args::Args::parse();

    if let Err(e) = log::init_log(args.clone()) {
        panic!("Failed to initialize logger: {}", e);
    }

    let kaspa_rpc_client =
        kaspad_client::connect(args.server.clone(), args.network())?;

    let service = service::KasWalletService::new(args.clone(), kaspa_rpc_client);
    let server = WalletServer::new(service);

    info!("Starting wallet server on {}", args.listen);

    Server::builder()
        .add_service(server)
        .serve(args.listen.parse().unwrap())
        .await?;

    Ok(())
}
