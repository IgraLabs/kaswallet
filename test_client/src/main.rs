use common::model::WalletSignableTransaction;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use kaspa_consensus_core::sign::Signed;
use kaspa_consensus_core::tx::SignableTransaction;
use kaswallet_client::client::KaswalletClient;
use kaswallet_client::model::{AddressBalance, BalanceInfo, TransactionBuilder};
use std::error::Error;
use tokio::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut client = KaswalletClient::connect("http://localhost:8082").await?;

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
            stress_test_parallel(&mut client).await?;
        }
        "get_utxos" => {
            println!("Running get_utxos test");
            get_utxos_test(&mut client).await?;
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

async fn get_utxos_test(client: &mut KaswalletClient) -> Result<(), Box<dyn Error + Send + Sync>> {
    let utxos = client.get_utxos(vec![], true, true).await?;
    println!("UTXOs: {:?}", utxos);
    Ok(())
}

const STRESS_TESTS_NUM_ITERATIONS: usize = 100;
async fn stress_test(client: &mut KaswalletClient) -> Result<(), Box<dyn Error + Send + Sync>> {
    let address = get_address_with_balance(client).await?;
    for _ in 0..STRESS_TESTS_NUM_ITERATIONS {
        test_send(client, &address, &address).await?
    }

    Ok(())
}

async fn stress_test_parallel(
    client: &mut KaswalletClient,
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

async fn mine_tx_id_test(client: &mut KaswalletClient) -> Result<(), Box<dyn Error + Send + Sync>> {
    let address = get_address_with_balance(client).await?;
    let actual_payload = b"hello igra!";
    let expected_bitmask: [u8; 2] = [0x97, 0xb1];

    let unsigned_transactions = TransactionBuilder::new(address.clone())
        .send_all()
        .payload(actual_payload.to_vec())
        .from_addresses(vec![address])
        .create_unsigned_transactions(client)
        .await?;

    let mut unsigned_transactions = unsigned_transactions;

    let transactions_count = unsigned_transactions.len();
    let last_transaction = &unsigned_transactions[transactions_count - 1];

    let mut wallet_transaction: WalletSignableTransaction = last_transaction.clone();

    let mut transaction_to_mine = wallet_transaction.transaction.unwrap();

    println!("Original transaction ID: {}", transaction_to_mine.id());

    mine_loop(&mut transaction_to_mine, expected_bitmask);

    wallet_transaction.transaction = Signed::Partially(transaction_to_mine);

    unsigned_transactions[transactions_count - 1] = wallet_transaction;

    let signed_transactions = client.sign(unsigned_transactions, "".to_string()).await?;
    println!("Transaction signed successfully");

    let transaction_ids = client.broadcast(signed_transactions).await?;
    println!(
        "Transaction broadcast successfully! Transaction IDs: {:?}",
        transaction_ids
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
    client: &mut KaswalletClient,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let balance_info = test_get_balance(client).await?;
    let address_balance = balance_info
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

async fn sanity_test(client: &mut KaswalletClient) -> Result<(), Box<dyn Error + Send + Sync>> {
    test_version(client).await?;

    let addresses = test_get_addresses(client).await?;

    if addresses.is_empty() {
        new_address(client).await?;
    }

    let balance_info = test_get_balance(client).await?;

    if balance_info.available == 0 {
        println!("No available balance to transfer");
        return Ok(());
    }

    let from_address_balance_response = balance_info
        .address_balances
        .iter()
        .max_by_key(|address_balance| address_balance.available)
        .unwrap();

    let from_address = from_address_balance_response.address.clone();

    let to_address = addresses
        .iter()
        .find(|address| !address.to_string().eq(&from_address))
        .map(|address| address.to_string());
    let to_address = if let Some(to_address) = to_address {
        to_address
    } else {
        new_address(client).await?
    };

    let default_address_balance = AddressBalance {
        address: to_address.clone(),
        available: 0,
        pending: 0,
    };

    let to_address_balance_response = balance_info
        .address_balances
        .iter()
        .find(|address_balance| address_balance.address.eq(&to_address))
        .unwrap_or(&default_address_balance);

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
    client: &mut KaswalletClient,
    from_address: &str,
    to_address: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let send_result = TransactionBuilder::new(to_address.to_string())
        .send_all()
        .from_addresses(vec![from_address.to_string()])
        .send(client, "".to_string())
        .await?;

    println!(
        "Send response: transaction_ids={:?}",
        send_result.transaction_ids
    );
    Ok(())
}

async fn test_get_balance(
    client: &mut KaswalletClient,
) -> Result<BalanceInfo, Box<dyn Error + Send + Sync>> {
    let balance_info = client.get_balance(true).await?;
    println!(
        "Balance: Available={}, Pending={}",
        balance_info.available, balance_info.pending
    );
    for address_balance in &balance_info.address_balances {
        println!(
            "\tAddress={:?}; Available={}, Pending={}",
            address_balance.address, address_balance.available, address_balance.pending
        );
    }
    Ok(balance_info)
}

async fn test_get_addresses(
    client: &mut KaswalletClient,
) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
    let addresses = client.get_addresses().await?;
    for address in &addresses {
        println!("Address={:?}", address);
    }
    Ok(addresses)
}

async fn test_version(client: &mut KaswalletClient) -> Result<(), Box<dyn Error + Send + Sync>> {
    let version = client.get_version().await?;
    println!("Version={:?}", version);
    Ok(())
}

async fn new_address(client: &mut KaswalletClient) -> Result<String, Box<dyn Error + Send + Sync>> {
    let address = client.new_address().await?;
    println!("New Address={:?}", address);
    Ok(address)
}
