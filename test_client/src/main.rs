use kaswallet_proto::kaswallet_proto::wallet_client::WalletClient;
use kaswallet_proto::kaswallet_proto::{
    AddressBalances, GetAddressesRequest, GetVersionRequest, SendRequest, TransactionDescription,
};
use std::error::Error;
use tonic::transport::Channel;
use tonic::Request;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = WalletClient::connect("http://localhost:8082").await?;

    let response = client
        .get_version(Request::new(GetVersionRequest {}))
        .await?;

    println!("Version={:?}", response.into_inner().version);

    // new_address(&mut client).await?;

    let get_addresses_response = client
        .get_addresses(Request::new(GetAddressesRequest {}))
        .await?
        .into_inner();
    for address in &get_addresses_response.address {
        println!("Address={:?}", address);
    }

    let get_balance_response = &client
        .get_balance(Request::new(
            kaswallet_proto::kaswallet_proto::GetBalanceRequest {
                include_balance_per_address: true,
            },
        ))
        .await?
        .into_inner();
    println!(
        "Balance: Available={}, Pending={}",
        get_balance_response.available, get_balance_response.pending
    );
    for address_balance in &get_balance_response.address_balances {
        println!(
            "\tAddress={:?}; Available={}, Pending={}",
            address_balance.address, address_balance.available, address_balance.pending
        );
    }

    if get_balance_response.available == 0 {
        println!("No available balance to transfer");
        return Ok(());
    }
    let from_address_balance_response = get_balance_response
        .address_balances
        .iter()
        .max_by_key(|address_balance| address_balance.available)
        .unwrap();
    let from_address = from_address_balance_response.address.clone();
    let to_address = get_addresses_response
        .address
        .iter()
        .find(|address| !address.to_string().eq(&from_address))
        .map(|address| address.to_string());
    let to_address = if to_address.is_none() {
        new_address(&mut client).await?
    } else {
        to_address.unwrap()
    };
    let default_address_balances = &AddressBalances {
        address: to_address.clone(),
        available: 0,
        pending: 0,
    };
    let to_address_balance_response = get_balance_response
        .address_balances
        .iter()
        .find(|address_balance| address_balance.address.eq(&to_address))
        .unwrap_or(&default_address_balances);
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
                amount: 20000,
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

async fn new_address(client: &mut WalletClient<Channel>) -> Result<String, Box<dyn Error>> {
    let new_address_response = client
        .new_address(Request::new(
            kaswallet_proto::kaswallet_proto::NewAddressRequest {},
        ))
        .await?;
    let address = new_address_response.into_inner().address;
    println!("New Address={:?}", address);
    Ok(address)
}
