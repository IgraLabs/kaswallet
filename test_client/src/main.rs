use tonic::Request;
use wallet_proto::wallet_proto::wallet_client::WalletClient;
use wallet_proto::wallet_proto::{GetAddressesRequest, GetVersionRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = WalletClient::connect("http://localhost:8082").await?;

    let response = client
        .get_version(Request::new(GetVersionRequest {}))
        .await?;

    println!("Version={:?}", response.into_inner().version);

    // let new_address_response = client
    //     .new_address(Request::new(
    //         wallet_proto::wallet_proto::NewAddressRequest {},
    //     ))
    //     .await?;

    // println!(
    //     "New Address={:?}",
    //     new_address_response.into_inner().address
    // );

    let get_addresses_response = client
        .get_addresses(Request::new(GetAddressesRequest {}))
        .await?;
    for address in get_addresses_response.into_inner().address {
        println!("Address={:?}", address);
    }

    let get_balance_response = client
        .get_balance(Request::new(
            wallet_proto::wallet_proto::GetBalanceRequest {
                include_balance_per_address: true,
            },
        ))
        .await?;

    println!("Get Balance={:?}", get_balance_response.into_inner());

    Ok(())
}
