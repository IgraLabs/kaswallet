use clap::Parser;
use common::args::expand_path;
use ::log::info;
use std::error::Error;
use tonic::transport::Server;
use wallet_proto::wallet_proto::wallet_server::WalletServer;

mod args;
mod log;
mod service;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = args::Args::parse();

    if let Err(e) = log::init_log(args.clone()) {
        panic!("Failed to initialize logger: {}", e);
    }

    let service = service::KasWalletService::new(args.clone());
    let server = WalletServer::new(service);

    info!("Starting wallet server on {}", args.listen);

    Server::builder()
        .add_service(server)
        .serve(args.listen.parse().unwrap())
        .await?;

    Ok(())
}
