use tonic::Request;
use wallet_proto::wallet_proto::wallet_client::WalletClient;
use wallet_proto::wallet_proto::GetVersionRequest;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = WalletClient::connect("http://localhost:8082").await?;

    let response = client
        .get_version(Request::new(GetVersionRequest {}))
        .await?;

    println!("Version={:?}", response.into_inner().version);

    Ok(())
}
