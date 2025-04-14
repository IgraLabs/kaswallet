use futures::stream::FuturesUnordered;
use futures::StreamExt;
use kaswallet_proto::kaswallet_proto::wallet_client::WalletClient;
use kaswallet_proto::kaswallet_proto::{
    AddressBalances, GetAddressesRequest, GetAddressesResponse, GetBalanceResponse,
    GetVersionRequest, SendRequest, TransactionDescription,
};
use std::error::Error;
use tonic::transport::Channel;
use tonic::Request;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut client = WalletClient::connect("http://localhost:8082").await?;

    let scenario = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sanity".to_string());
    match scenario.as_str() {
        "sanity" => {
            println!("Running sanity test");
            sanity_test(&mut client).await?
        }
        "stress" => {
            println!("Running stress test");
            stress_test(&mut client).await?
        }
        "parallel" => {
            println!("Running stress test in parallel");
            stress_test_parallel(&mut client).await?
        }
        _ => {
            return Err(format!("Unknown scenario {}", scenario).into());
        }
    }

    Ok(())
}

const STRESS_TESTS_NUM_ITERATIONS: usize = 1000;
async fn stress_test(client: &mut WalletClient<Channel>) -> Result<(), Box<dyn Error>> {
    let address = prepare_stress_test(client).await?;
    for _ in 0..STRESS_TESTS_NUM_ITERATIONS {
        test_send(client, &address, &address).await?
    }

    Ok(())
}

async fn stress_test_parallel(client: &mut WalletClient<Channel>) -> Result<(), Box<dyn Error>> {
    let address = prepare_stress_test(client).await?;
    let mut futures = FuturesUnordered::new();
    for _ in 0..STRESS_TESTS_NUM_ITERATIONS {
        let mut client = client.clone();
        let address = address.clone();
        let future = async move { test_send(&mut client, &address, &address).await };
        futures.push(future);
    }

    let mut iterations_finished = 0;
    while let Some(result) = futures.next().await {
        match result {
            Ok(_) => println!("Send success"),
            Err(e) => println!("Send error: {:?}", e),
        };
        iterations_finished += 1;
        println!(
            "Completed {} out of {} iterations",
            iterations_finished, STRESS_TESTS_NUM_ITERATIONS
        );
    }

    Ok(())
}

// returns address
async fn prepare_stress_test(client: &mut WalletClient<Channel>) -> Result<String, Box<dyn Error>> {
    let balances_response = test_get_ballance(client).await?;
    let address_balance = balances_response
        .address_balances
        .iter()
        .filter(|ab| ab.available > 0)
        .next();
    if address_balance.is_none() {
        return Err("No available balance to transfer".into());
    }
    let address_balance = address_balance.unwrap();

    println!(
        "Running with address {} which has balance of {}",
        address_balance.address, address_balance.available
    );
    Ok(address_balance.address.clone())
}

async fn sanity_test(client: &mut WalletClient<Channel>) -> Result<(), Box<dyn Error>> {
    test_version(client).await?;

    let get_addresses_response = test_get_addresses(client).await?;

    if get_addresses_response.address.len() == 0 {
        new_address(client).await?;
    }

    let get_balance_response = test_get_ballance(client).await?;

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
        new_address(client).await?
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

    test_send(client, &from_address, &to_address).await?;

    Ok(())
}

async fn test_send(
    client: &mut WalletClient<Channel>,
    from_address: &String,
    to_address: &String,
) -> Result<(), Box<dyn Error>> {
    let send_response = &client
        .send(Request::new(SendRequest {
            transaction_description: Some(TransactionDescription {
                to_address: to_address.clone(),
                amount: 0,
                is_send_all: true,
                payload: vec![],
                from_addresses: vec![from_address.clone()],
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

async fn test_get_ballance(
    client: &mut WalletClient<Channel>,
) -> Result<GetBalanceResponse, Box<dyn Error>> {
    let get_balance_response = client
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
    Ok(get_balance_response)
}

async fn test_get_addresses(
    client: &mut WalletClient<Channel>,
) -> Result<GetAddressesResponse, Box<dyn Error>> {
    let get_addresses_response = client
        .get_addresses(Request::new(GetAddressesRequest {}))
        .await?
        .into_inner();
    for address in &get_addresses_response.address {
        println!("Address={:?}", address);
    }
    Ok(get_addresses_response)
}

async fn test_version(client: &mut WalletClient<Channel>) -> Result<(), Box<dyn Error>> {
    let response = client
        .get_version(Request::new(GetVersionRequest {}))
        .await?;

    println!("Version={:?}", response.into_inner().version);
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
