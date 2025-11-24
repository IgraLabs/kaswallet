use common::model::WalletSignableTransaction;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use kaspa_consensus_core::sign::Signed;
use kaspa_consensus_core::tx::SignableTransaction;
use proto::kaswallet_proto::wallet_client::WalletClient;
use proto::kaswallet_proto::{
    AddressBalances, BroadcastRequest, CreateUnsignedTransactionsRequest, GetAddressesRequest,
    GetAddressesResponse, GetBalanceRequest, GetBalanceResponse, GetVersionRequest,
    NewAddressRequest, SendRequest, SignRequest, TransactionDescription,
};
use std::error::Error;
use tokio::time::Instant;
use tonic::Request;
use tonic::codegen::Bytes;
use tonic::transport::channel::Channel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
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
        "mine" => {
            println!("Running mine tx id test");
            mine_tx_id_test(&mut client).await?;
        }
        _ => {
            return Err(format!("Unknown scenario {}", scenario).into());
        }
    }

    Ok(())
}

const STRESS_TESTS_NUM_ITERATIONS: usize = 100;
async fn stress_test(
    client: &mut WalletClient<Channel>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let address = get_address_with_balance(client).await?;
    for _ in 0..STRESS_TESTS_NUM_ITERATIONS {
        test_send(client, &address, &address).await?
    }

    Ok(())
}

async fn stress_test_parallel(
    client: &mut WalletClient<Channel>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let address = get_address_with_balance(client).await?;
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

async fn mine_tx_id_test(
    client: &mut WalletClient<Channel>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let address = get_address_with_balance(client).await?;
    let actual_payload = b"hello igra!";
    let expected_bitmask: [u8; 2] = [0x97, 0xb1];
    let create_unsigned_transaction_request = CreateUnsignedTransactionsRequest {
        transaction_description: Some(TransactionDescription {
            to_address: address.clone(),
            amount: 0,
            is_send_all: true,
            payload: actual_payload.to_vec().into(),
            from_addresses: vec![address],
            utxos: vec![],
            use_existing_change_address: false,
            fee_policy: None,
        }),
    };
    let create_unsigned_transaction_response = client
        .create_unsigned_transactions(create_unsigned_transaction_request)
        .await?
        .into_inner();

    let mut unsinged_transactions = create_unsigned_transaction_response.unsigned_transactions;

    let transactions_count = unsinged_transactions.len();
    let last_transaction = &unsinged_transactions[transactions_count - 1];

    let mut wallet_transaction: WalletSignableTransaction = last_transaction.clone().into();

    let mut transaction_to_mine = wallet_transaction.transaction.unwrap();

    println!("Original transaction ID: {}", transaction_to_mine.id());

    mine_loop(&mut transaction_to_mine, expected_bitmask);

    wallet_transaction.transaction = Signed::Partially(transaction_to_mine);

    unsinged_transactions[transactions_count - 1] = wallet_transaction.into();

    let sign_request = SignRequest {
        unsigned_transactions: unsinged_transactions,
        password: "".to_string(),
    };
    let sign_reponse = client.sign(sign_request).await?;
    println!("Transaction signed successfully");

    let broadcast_request = BroadcastRequest {
        transactions: sign_reponse.into_inner().signed_transactions,
    };

    let broadcast_response = client.broadcast(broadcast_request).await?;
    println!(
        "Transaction broadcast successfully! Broadcast response: {:?}",
        broadcast_response
    );

    Ok(())
}

fn mine_loop(transaction_to_mine: &mut SignableTransaction, expected_bitmask: [u8; 2]) {
    let start = Instant::now();

    let bitmask_length = expected_bitmask.len();
    let mut nonce: u64 = 0;

    let original_payload = transaction_to_mine.tx.payload.clone();
    let mut new_payload = original_payload.clone();
    new_payload.extend_from_slice(&nonce.to_le_bytes());

    loop {
        let len = new_payload.len();
        new_payload[len - 8..].copy_from_slice(&nonce.to_le_bytes());

        transaction_to_mine.tx.payload = new_payload.clone();

        transaction_to_mine.tx.finalize(); // this updates the transaction ID

        let transaction_id = transaction_to_mine.id();
        if transaction_id.as_bytes()[..bitmask_length] == expected_bitmask {
            println!(
                "Found transaction ID {} with payload {:?} and nonce {}({:?})",
                transaction_id,
                new_payload.clone(),
                nonce,
                nonce.to_le_bytes()
            );

            let duration = start.elapsed();
            println!(
                "mine loop took: {:?}, at {} hashes/sec",
                duration,
                nonce * 1000 / duration.as_millis() as u64
            );
            break;
        }
        nonce += 1;
        // This means we tested all possible nonces and got no valid result
        if nonce == 0 {
            println!(
                "Exhausted all possible nonces without finding a valid transaction ID; This should happen extremely rarely"
            );
            transaction_to_mine.tx.outputs[0].value -= 1; // Decrease the output value to create variance
        }
    }
}

// returns address
async fn get_address_with_balance(
    client: &mut WalletClient<Channel>,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let balances_response = test_get_ballance(client).await?;
    let address_balance = balances_response
        .address_balances
        .iter()
        .find(|ab| ab.available > 0);
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

async fn sanity_test(
    client: &mut WalletClient<Channel>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    test_version(client).await?;

    let get_addresses_response = test_get_addresses(client).await?;

    if get_addresses_response.address.is_empty() {
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
    let to_address = if let Some(to_address) = to_address {
        to_address
    } else {
        new_address(client).await?
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
        .unwrap_or(default_address_balances);

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
    from_address: &str,
    to_address: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let send_response = &client
        .send(Request::new(SendRequest {
            transaction_description: Some(TransactionDescription {
                to_address: to_address.to_string(),
                amount: 0,
                is_send_all: true,
                payload: Bytes::new(),
                from_addresses: vec![from_address.to_string()],
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
) -> Result<GetBalanceResponse, Box<dyn Error + Send + Sync>> {
    let get_balance_response = client
        .get_balance(Request::new(GetBalanceRequest {
            include_balance_per_address: true,
        }))
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
) -> Result<GetAddressesResponse, Box<dyn Error + Send + Sync>> {
    let get_addresses_response = client
        .get_addresses(Request::new(GetAddressesRequest {}))
        .await?
        .into_inner();
    for address in &get_addresses_response.address {
        println!("Address={:?}", address);
    }
    Ok(get_addresses_response)
}

async fn test_version(
    client: &mut WalletClient<Channel>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let response = client
        .get_version(Request::new(GetVersionRequest {}))
        .await?;

    println!("Version={:?}", response.into_inner().version);
    Ok(())
}

async fn new_address(
    client: &mut WalletClient<Channel>,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let new_address_response = client
        .new_address(Request::new(NewAddressRequest {}))
        .await?;
    let address = new_address_response.into_inner().address;
    println!("New Address={:?}", address);
    Ok(address)
}
