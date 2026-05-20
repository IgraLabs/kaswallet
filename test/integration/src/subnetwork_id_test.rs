use kaspa_consensus_core::config::params::SIMNET_PARAMS;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaswallet_client::client::KaswalletClient;
use kaswallet_client::model::TransactionBuilder;
use kaswallet_daemon::log::init_log_for_tests;
use kaswallet_test_helpers::mine_block::mine_block;
use kaswallet_test_helpers::mnemonics::create_known_test_mnemonic;
use kaswallet_test_helpers::start_daemon::{start_kaspad, start_wallet_daemon_with_subnetwork_id};
use rstest::rstest;
use std::str::FromStr;
use std::time::Duration;
use tokio::time::sleep;

const IGRA_LANE_ID_HEX: &str = "97b1000000000000000000000000000000000000";

#[rstest]
#[tokio::test]
pub async fn test_send_uses_configured_subnetwork_id() {
    init_log_for_tests();
    let mnemonic = create_known_test_mnemonic();

    let (_keys, keys_file_path) =
        kaswallet_test_helpers::create::create_keys_file(mnemonic).unwrap();
    let (_kaspad_daemon, kaspad_client) = start_kaspad().await;
    sleep(Duration::from_millis(500)).await;

    let (_wallet_daemon, listen) = start_wallet_daemon_with_subnetwork_id(
        kaspad_client.clone(),
        keys_file_path,
        IGRA_LANE_ID_HEX,
    )
    .await;
    sleep(Duration::from_millis(1000)).await;
    let mut wallet_client = KaswalletClient::connect(&format!("grpc://{}", listen))
        .await
        .unwrap();

    let subsidy = SIMNET_PARAMS.pre_deflationary_phase_base_subsidy;
    let null_address = "kaspasim:qzvclevegss9de2hr48jszg59vemc9nedxkyfxusryhra2kjyfcu2uwk0sdyg";

    let from_address = wallet_client.new_address().await.expect("from address");
    let to_address = wallet_client.new_address().await.expect("to address");

    mine_block(kaspad_client.clone(), &from_address).await;
    mine_block(kaspad_client.clone(), null_address).await;
    sleep(Duration::from_millis(3000)).await;

    let balance = wallet_client.get_balance(true).await.expect("balance");
    assert_eq!(balance.available, subsidy);

    let send_amount = subsidy / 2;
    TransactionBuilder::new(to_address.to_string())
        .amount(send_amount)
        .from_addresses(vec![from_address.to_string()])
        .send(&mut wallet_client, "".to_string())
        .await
        .expect("send transaction");

    let block = mine_block(kaspad_client, null_address).await;
    sleep(Duration::from_millis(3000)).await;

    let expected = SubnetworkId::from_str(IGRA_LANE_ID_HEX).expect("test constant is valid hex");

    let tx = block
        .transactions
        .iter()
        .find(|tx| tx.subnetwork_id == expected)
        .expect("outgoing tx with the configured subnetwork_id must be in the block");

    // Sanity: the tx actually moved funds — there should be ≥1 input and ≥1 output.
    assert!(!tx.inputs.is_empty(), "tx has no inputs");
    assert!(!tx.outputs.is_empty(), "tx has no outputs");
}
