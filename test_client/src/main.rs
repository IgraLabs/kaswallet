use kaspa_consensus_core::constants::SOMPI_PER_KASPA;
use tonic::Request;
use wallet_proto::wallet_proto::wallet_client::WalletClient;
use wallet_proto::wallet_proto::{
    GetAddressesRequest, GetVersionRequest, SendRequest, TransactionDescription,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = WalletClient::connect("http://localhost:8082").await?;

    let response = client
        .get_version(Request::new(GetVersionRequest {}))
        .await?;

    println!("Version={:?}", response.into_inner().version);

    //let new_address_response = client
    //    .new_address(Request::new(
    //        wallet_proto::wallet_proto::NewAddressRequest {},
    //    ))
    //    .await?;

    //println!(
    //    "New Address={:?}",
    //    new_address_response.into_inner().address
    //);

    let get_addresses_response = client
        .get_addresses(Request::new(GetAddressesRequest {}))
        .await?
        .into_inner();
    for address in &get_addresses_response.address {
        println!("Address={:?}", address);
    }

    let get_balance_response = &client
        .get_balance(Request::new(
            wallet_proto::wallet_proto::GetBalanceRequest {
                include_balance_per_address: true,
            },
        ))
        .await?
        .into_inner();
    println!(
        "Balance: Available={}, Pending={}",
        get_balance_response.available, get_balance_response.pending
    );

    if get_balance_response.available == 0 {
        println!("No available balance to transfer");
        return Ok(());
    }
    let from_address_balance_response = get_balance_response.address_balances[0].clone();
    let from_address = from_address_balance_response.address;
    let to_address_balance_response = get_balance_response
        .address_balances
        .iter()
        .find(|address_balance| address_balance.available > 0);
    let to_address_balance_response = to_address_balance_response.unwrap();
    let to_address = to_address_balance_response.address.clone();
    println!(
        "FromAddress={:?}; Balance: {}",
        from_address, from_address_balance_response.available
    );
    println!(
        "ToAddress={:?}; Balance: {}",
        to_address, to_address_balance_response.available
    );

    let send_response = &client
        .send(Request::new(SendRequest {
            transaction_description: Some(TransactionDescription {
                to_address,
                amount: 100 * SOMPI_PER_KASPA,
                is_send_all: false,
                payload: vec![],
                from_addresses: vec![from_address],
                utxos: vec![],
                use_existing_change_address: false,
                fee_policy: None,
            }),
            password: "".to_string(),
        }))
        .await?
        .into_inner();
    println!("Send response={:?}", send_response);

    Ok(())
}
