use kaspa_consensus_core::config::params::SIMNET_PARAMS;
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaswallet_client::client::KaswalletClient;
use kaswallet_client::model::TransactionBuilder;
use kaswallet_daemon::args::LogsLevel;
use kaswallet_daemon::log::init_log;
use kaswallet_test_helpers::mine_block::mine_block;
use kaswallet_test_helpers::mnemonics::create_known_test_mnemonic;
use kaswallet_test_helpers::start_daemon::{start_kaspad, start_wallet_daemon};
use rstest::rstest;
use std::time::Duration;
use tempfile::tempdir;
use tokio::time::sleep;

#[rstest]
#[tokio::test]
pub async fn test_p2pk_send() {
    init_log(
        tempdir().unwrap().path().to_str().unwrap(),
        &LogsLevel::Info,
    )
    .expect("Failed to initialize logger");
    let mnemnonic = create_known_test_mnemonic();

    let (_keys, keys_file_path) =
        kaswallet_test_helpers::create::create_keys_file(mnemnonic).unwrap();
    let (mut kaspad_daemon, kaspad_client) = start_kaspad().await;
    sleep(Duration::from_millis(500)).await; // Give kaspad some time to start properly

    let (_wallet_daemon, listen) = start_wallet_daemon(kaspad_client.clone(), keys_file_path).await;
    sleep(Duration::from_millis(1000)).await; // Give wallet some time to start and sync
    let mut wallet_client = KaswalletClient::connect(format!("grpc://{}", listen))
        .await
        .unwrap();

    let subsidy = SIMNET_PARAMS.pre_deflationary_phase_base_subsidy;

    let null_address = "kaspasim:qzvclevegss9de2hr48jszg59vemc9nedxkyfxusryhra2kjyfcu2uwk0sdyg";

    let from_address = wallet_client
        .new_address()
        .await
        .expect("Failed to get from address");
    let to_address = wallet_client
        .new_address()
        .await
        .expect("Failed to get to address");

    mine_block(kaspad_client.clone(), &from_address).await;
    mine_block(kaspad_client.clone(), null_address).await;
    sleep(Duration::from_millis(3000)).await; // Give wallet time to sync

    let balance = wallet_client
        .get_balance(true)
        .await
        .expect("Failed to get balance");
    assert_eq!(balance.available, subsidy);
    let from_address_balance = balance
        .address_balances
        .iter()
        .find(|b| b.address == from_address)
        .unwrap();
    assert_eq!(from_address_balance.available, subsidy);

    let send_amount = subsidy / 2;
    TransactionBuilder::new(to_address.to_string())
        .amount(send_amount)
        .from_addresses(vec![from_address.to_string()])
        .send(&mut wallet_client, "".to_string())
        .await
        .expect("Failed to send transaction");

    let block = mine_block(kaspad_client, null_address).await;
    sleep(Duration::from_millis(3000)).await; // Give wallet time to sync

    let transaction = block
        .transactions
        .iter()
        .find(|tx| tx.subnetwork_id == SUBNETWORK_ID_NATIVE)
        .unwrap();
    let change_value = transaction.outputs[1].value;
    let balance = wallet_client
        .get_balance(true)
        .await
        .expect("Failed to get balance");

    assert_eq!(balance.available, subsidy / 2 + change_value);
    let from_address_balance = balance
        .address_balances
        .iter()
        .find(|b| b.address == from_address)
        .unwrap();
    let to_address_balance = balance
        .address_balances
        .iter()
        .find(|b| b.address == to_address)
        .unwrap();
    assert_eq!(from_address_balance.available, change_value);
    assert_eq!(to_address_balance.available, subsidy / 2);

    // Send all to null address to clean wallet
    TransactionBuilder::new(null_address.to_string())
        .send_all()
        .send(&mut wallet_client, "".to_string())
        .await
        .expect("Failed to send transaction");
    let balance = wallet_client
        .get_balance(true)
        .await
        .expect("Failed to get balance");
    assert_eq!(balance.available, 0);

    kaspad_daemon.shutdown();
}

#[rstest]
#[tokio::test]
pub async fn test_p2pk_create_sign_broadcast() {
    init_log(
        tempdir().unwrap().path().to_str().unwrap(),
        &LogsLevel::Info,
    )
    .expect("Failed to initialize logger");
    let mnemnonic = create_known_test_mnemonic();

    let (_keys, keys_file_path) =
        kaswallet_test_helpers::create::create_keys_file(mnemnonic).unwrap();
    let (mut kaspad_daemon, kaspad_client) = start_kaspad().await;
    sleep(Duration::from_millis(500)).await; // Give kaspad some time to start properly

    let (_wallet_daemon, listen) = start_wallet_daemon(kaspad_client.clone(), keys_file_path).await;
    sleep(Duration::from_millis(1000)).await; // Give wallet some time to start and sync
    let mut wallet_client = KaswalletClient::connect(format!("grpc://{}", listen))
        .await
        .unwrap();

    let subsidy = SIMNET_PARAMS.pre_deflationary_phase_base_subsidy;

    let null_address = "kaspasim:qzvclevegss9de2hr48jszg59vemc9nedxkyfxusryhra2kjyfcu2uwk0sdyg";

    let from_address = wallet_client
        .new_address()
        .await
        .expect("Failed to get from address");
    let to_address = wallet_client
        .new_address()
        .await
        .expect("Failed to get to address");

    mine_block(kaspad_client.clone(), &from_address).await;
    mine_block(kaspad_client.clone(), null_address).await;
    sleep(Duration::from_millis(3000)).await; // Give wallet time to sync

    let balance = wallet_client
        .get_balance(true)
        .await
        .expect("Failed to get balance");
    assert_eq!(balance.available, subsidy);
    let from_address_balance = balance
        .address_balances
        .iter()
        .find(|b| b.address == from_address)
        .unwrap();
    assert_eq!(from_address_balance.available, subsidy);
    let send_amount = subsidy / 2;
    let unsigned_transactions = TransactionBuilder::new(to_address.to_string())
        .amount(send_amount)
        .from_addresses(vec![from_address.to_string()])
        .create_unsigned_transactions(&mut wallet_client)
        .await
        .expect("Failed to send transaction");

    let signed_transactions = wallet_client
        .sign(unsigned_transactions, "".to_string())
        .await
        .expect("Failed to sign transaction");

    wallet_client.broadcast(signed_transactions).await.unwrap();

    let block = mine_block(kaspad_client, null_address).await;
    sleep(Duration::from_millis(3000)).await; // Give wallet time to sync

    let transaction = block
        .transactions
        .iter()
        .find(|tx| tx.subnetwork_id == SUBNETWORK_ID_NATIVE)
        .unwrap();
    let change_value = transaction.outputs[1].value;
    let balance = wallet_client
        .get_balance(true)
        .await
        .expect("Failed to get balance");

    assert_eq!(balance.available, subsidy / 2 + change_value);
    let from_address_balance = balance
        .address_balances
        .iter()
        .find(|b| b.address == from_address)
        .unwrap();
    let to_address_balance = balance
        .address_balances
        .iter()
        .find(|b| b.address == to_address)
        .unwrap();
    assert_eq!(from_address_balance.available, change_value);
    assert_eq!(to_address_balance.available, subsidy / 2);

    // Send all to null address to clean wallet
    TransactionBuilder::new(null_address.to_string())
        .send_all()
        .send(&mut wallet_client, "".to_string())
        .await
        .expect("Failed to send transaction");
    let balance = wallet_client
        .get_balance(true)
        .await
        .expect("Failed to get balance");
    assert_eq!(balance.available, 0);

    kaspad_daemon.shutdown();
}
