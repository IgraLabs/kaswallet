use kaspa_addresses::Address;
use kaspa_consensus_core::config::params::SIMNET_PARAMS;
use kaspa_consensus_core::constants::{TX_VERSION, TX_VERSION_TOCCATA};
use kaspa_consensus_core::subnets::{SUBNETWORK_ID_NATIVE, SubnetworkId};
use kaspa_consensus_core::tx::Transaction;
use kaspa_txscript::pay_to_address_script;
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

/// IGRA user-lane 4-byte namespace per KIP-21. The wallet zero-pads this to
/// the full 20-byte SubnetworkId (`97b10000` followed by 16 zero bytes).
pub const IGRA_LANE_NAMESPACE_HEX: &str = "97b10000";

/// All-zero namespace resolved as the native subnetwork.
const NATIVE_NAMESPACE_HEX: &str = "00000000";

#[rstest]
#[tokio::test]
pub async fn test_send_uses_configured_non_native_subnetwork_id() {
    init_log_for_tests();
    let expected_subnetwork = SubnetworkId::from_str("97b1000000000000000000000000000000000000")
        .expect("padded namespace is valid hex");
    let expected_version = TX_VERSION_TOCCATA;

    run_send_assertions(
        IGRA_LANE_NAMESPACE_HEX,
        expected_subnetwork,
        expected_version,
    )
    .await;
}

#[rstest]
#[tokio::test]
pub async fn test_send_with_explicit_native_subnetwork_id_uses_tx_version_zero() {
    init_log_for_tests();
    run_send_assertions(NATIVE_NAMESPACE_HEX, SUBNETWORK_ID_NATIVE, TX_VERSION).await;
}

// TODO: extend with an auto-compound case (split / merge paths) once we have a
// fixture that can stage enough UTXOs to exceed MAXIMUM_STANDARD_TRANSACTION_MASS.
// Both `create_split_transaction` and `merge_transaction` recurse through
// `generate_unsigned_transaction`, which carries `self.tx_version` and
// `self.subnetwork_id`, so the field propagates by construction — this test
// would lock that in against future refactors.
//
// TODO: once IgraLabs/rusty-kaspa rebases onto the upstream Toccata branch,
// expand into an end-to-end test: create → sign → broadcast → mine →
// verify acceptance in the configured IGRA lane (not just block inclusion).
async fn run_send_assertions(
    subnetwork_namespace_hex: &str,
    expected_subnetwork: SubnetworkId,
    expected_version: u16,
) {
    let mnemonic = create_known_test_mnemonic();

    let (_keys, keys_file_path) =
        kaswallet_test_helpers::create::create_keys_file(mnemonic).unwrap();
    let (_kaspad_daemon, kaspad_client) = start_kaspad().await;
    sleep(Duration::from_millis(500)).await;

    let (_wallet_daemon, listen) = start_wallet_daemon_with_subnetwork_id(
        kaspad_client.clone(),
        keys_file_path,
        subnetwork_namespace_hex,
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
    let send_result = TransactionBuilder::new(to_address.to_string())
        .amount(send_amount)
        .from_addresses(vec![from_address.to_string()])
        .send(&mut wallet_client, "".to_string())
        .await
        .expect("send transaction");
    let expected_tx_id = send_result
        .transaction_ids
        .first()
        .copied()
        .expect("send must return at least one transaction id");

    let block = mine_block(kaspad_client, null_address).await;
    sleep(Duration::from_millis(3000)).await;

    // Locate the wallet's outgoing transaction by id, not by subnetwork id alone:
    // a future kaspad system tx, or an unrelated wallet emission, must not be
    // able to satisfy the assertions below. RpcTransaction does not expose `.id()`
    // directly — convert each candidate into a finalized consensus Transaction
    // to derive the id.
    let tx = block
        .transactions
        .iter()
        .find(|rpc_tx| {
            Transaction::try_from((*rpc_tx).clone())
                .map(|t| t.id() == expected_tx_id)
                .unwrap_or(false)
        })
        .expect("wallet's send transaction must appear in the mined block");

    assert_eq!(
        tx.subnetwork_id, expected_subnetwork,
        "tx must carry the configured subnetwork id"
    );
    assert_eq!(
        tx.version, expected_version,
        "tx version must match the version selected for this subnetwork",
    );

    // The output set must include a payment of `send_amount` to `to_address`.
    let to_address_parsed =
        Address::try_from(to_address.clone()).expect("to_address must be a valid Kaspa address");
    let expected_script = pay_to_address_script(&to_address_parsed);
    let paid_to_recipient = tx
        .outputs
        .iter()
        .any(|o| o.script_public_key == expected_script && o.value == send_amount);
    assert!(
        paid_to_recipient,
        "tx must contain an output paying send_amount={send_amount} to to_address",
    );

    // Sanity: the tx actually consumed an input (i.e., spends a UTXO from the wallet).
    assert!(!tx.inputs.is_empty(), "tx has no inputs");

    // v1 (Toccata) inputs must carry compute_budget, v0 inputs must carry
    // sig_op_count. The RpcTransaction view carries the post-conversion
    // consensus Transaction, so each input's `compute_commit` field is authoritative.
    let consensus_tx =
        Transaction::try_from(tx.clone()).expect("rpc tx must round-trip into consensus tx");
    for (i, input) in consensus_tx.inputs.iter().enumerate() {
        if expected_version >= 1 {
            assert!(
                input.compute_commit.compute_budget().is_some(),
                "v{expected_version} input #{i} must carry compute_budget"
            );
            assert!(
                input.compute_commit.sig_op_count().is_none(),
                "v{expected_version} input #{i} must not carry sig_op_count"
            );
        } else {
            assert!(
                input.compute_commit.sig_op_count().is_some(),
                "v0 input #{i} must carry sig_op_count"
            );
        }
    }

    // Provenance: at least one signed transaction was emitted and its
    // input-address mapping is non-empty — i.e., inputs are owned by wallet
    // addresses, not unrelated UTXOs.
    let signed = send_result
        .signed_transactions
        .first()
        .expect("send must produce at least one signed transaction");
    assert!(
        !signed.address_by_input_index.is_empty(),
        "signed transaction must have at least one wallet-owned input address",
    );
}
