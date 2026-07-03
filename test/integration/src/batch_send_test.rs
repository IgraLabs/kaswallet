use batch_send::args::Args;
use batch_send::run_with_client_and_params;
use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::config::params::SIMNET_PARAMS;
use kaspa_consensus_core::constants::SOMPI_PER_KASPA;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaswallet_daemon::address_manager::AddressManager;
use kaswallet_daemon::log::init_log_for_tests;
use kaswallet_test_helpers::mine_block::mine_block;
use kaswallet_test_helpers::mnemonics::create_known_test_mnemonic;
use kaswallet_test_helpers::start_daemon::start_kaspad;
use rstest::rstest;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

const NULL_ADDRESS: &str = "kaspasim:qzvclevegss9de2hr48jszg59vemc9nedxkyfxusryhra2kjyfcu2uwk0sdyg";

fn batch_send_args(keys_file_path: &str, outputs: Vec<String>) -> Args {
    Args {
        simnet: true,
        keys_file_path: Some(keys_file_path.to_string()),
        // Keys files created by the test helpers use an empty password.
        password: Some(String::new()),
        outputs,
        // The isolated simnet kaspad runs with `enable_unsynced_mining` and
        // reports `is_synced = false`.
        allow_unsynced_node: true,
        ..Args::default()
    }
}

// Recipient addresses deliberately do NOT belong to the sender wallet —
// UTXO selection is smallest-first, so recipients inside the same wallet
// would get re-selected as inputs by later operations and muddy asserts.
fn recipient_address(seed: u8) -> Address {
    Address::new(Prefix::Simnet, Version::PubKey, &[seed; 32])
}

async fn recipient_amount_sompi(
    kaspad_client: &Arc<kaspa_grpc_client::GrpcClient>,
    address: &Address,
) -> u64 {
    kaspad_client
        .get_utxos_by_addresses(vec![address.clone()])
        .await
        .expect("get_utxos_by_addresses failed")
        .iter()
        .map(|entry| entry.utxo_entry.amount)
        .sum()
}

#[rstest]
#[tokio::test]
pub async fn test_batch_send() {
    init_log_for_tests();
    let mnemonic = create_known_test_mnemonic();
    let (keys, keys_file_path) =
        kaswallet_test_helpers::create::create_keys_file(mnemonic).unwrap();

    let (mut kaspad_daemon, kaspad_client) = start_kaspad().await;
    sleep(Duration::from_millis(500)).await; // Give kaspad some time to start properly

    // The wallet daemon is intentionally NOT started — the tool under test
    // talks straight to kaspad. Fund a wallet address derived locally with
    // TWO coinbase UTXOs (one 50-KAS subsidy each) so the batch send must
    // select multiple inputs — exercising the selection stop rules and the
    // per-iteration fee re-estimation, not just the single-input path.
    let address_manager = AddressManager::new(Arc::new(keys), Prefix::Simnet);
    let (funding_address, _) = address_manager
        .new_address()
        .await
        .expect("failed to derive funding address");
    mine_block(kaspad_client.clone(), &funding_address).await;
    mine_block(kaspad_client.clone(), &funding_address).await;
    // Coinbase UTXOs enter the UTXO index once the next block accepts them.
    mine_block(kaspad_client.clone(), NULL_ADDRESS).await;
    sleep(Duration::from_millis(1000)).await;

    let recipients = [
        recipient_address(1),
        recipient_address(2),
        recipient_address(3),
    ];
    // 83.5 KAS total exceeds a single 50-KAS coinbase UTXO → two inputs.
    let amounts_kas = ["60", "11.5", "12"];
    let amounts_sompi: [u64; 3] = [
        60 * SOMPI_PER_KASPA,
        11 * SOMPI_PER_KASPA + SOMPI_PER_KASPA / 2,
        12 * SOMPI_PER_KASPA,
    ];
    let outputs: Vec<String> = recipients
        .iter()
        .zip(amounts_kas)
        .map(|(address, amount)| format!("{address}:{amount}"))
        .collect();

    let mut params = SIMNET_PARAMS.clone();
    // Match the kaspad fixture (`fixtures/override_params.json`), same as
    // the wallet-daemon test harness does.
    params.coinbase_maturity = 0;

    // --- Case 0: --show-address data — offline address + synced balance ---
    let status_args = batch_send_args(&keys_file_path, vec![]);
    let receive_address = batch_send::show_receive_address(&status_args)
        .await
        .expect("offline address derivation failed");
    assert_eq!(receive_address.prefix, Prefix::Simnet);
    let subsidy = SIMNET_PARAMS.pre_deflationary_phase_base_subsidy;
    let balance = batch_send::wallet_balance_with_client_and_params(
        &status_args,
        kaspad_client.clone(),
        params.clone(),
    )
    .await
    .expect("balance fetch failed");
    assert_eq!(balance.total_sompi, 2 * subsidy);
    assert_eq!(balance.spendable_sompi, 2 * subsidy);

    // --- Case 1: dry run builds a full plan but submits nothing; the
    //     recipients come from a JSON outputs file (map of address ->
    //     amount-KAS strings) to cover the --outputs-file path end to end ---
    let outputs_file = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    let json_entries: Vec<String> = recipients
        .iter()
        .zip(amounts_kas)
        .map(|(address, amount)| format!("\"{address}\": \"{amount}\""))
        .collect();
    std::fs::write(
        outputs_file.path(),
        format!("{{{}}}", json_entries.join(", ")),
    )
    .unwrap();
    let dry_run_args = Args {
        dry_run: true,
        outputs_file: Some(outputs_file.path().to_string_lossy().to_string()),
        ..batch_send_args(&keys_file_path, vec![])
    };
    let summary = run_with_client_and_params(dry_run_args, kaspad_client.clone(), params.clone())
        .await
        .expect("dry run failed");
    assert!(summary.dry_run);
    assert!(!summary.transaction_ids.is_empty());
    assert_eq!(summary.recipients, 3);
    assert_eq!(summary.paid.len(), 3);
    assert!(summary.unpaid.is_empty());
    assert_eq!(summary.requested_sompi, amounts_sompi.iter().sum::<u64>());
    assert!(summary.fee_sompi > 0);
    mine_block(kaspad_client.clone(), NULL_ADDRESS).await;
    sleep(Duration::from_millis(500)).await;
    for recipient in &recipients {
        assert_eq!(
            recipient_amount_sompi(&kaspad_client, recipient).await,
            0,
            "dry run must not submit anything"
        );
    }

    // --- Case 2: real send pays every recipient exactly once ---
    let summary = run_with_client_and_params(
        batch_send_args(&keys_file_path, outputs.clone()),
        kaspad_client.clone(),
        params.clone(),
    )
    .await
    .expect("batch send failed");
    assert!(!summary.dry_run);
    assert_eq!(summary.paid.len(), 3);
    assert!(summary.unpaid.is_empty());
    assert!(summary.failure.is_none());
    assert!(summary.fee_sompi > 0);

    mine_block(kaspad_client.clone(), NULL_ADDRESS).await;
    sleep(Duration::from_millis(1000)).await;
    for (recipient, expected_sompi) in recipients.iter().zip(amounts_sompi) {
        assert_eq!(
            recipient_amount_sompi(&kaspad_client, recipient).await,
            expected_sompi,
            "recipient {recipient} must hold exactly the sent amount"
        );
    }

    // --- Case 3: forced chaining — max_outputs_per_tx = 1 makes each of two
    //     fresh recipients its own transaction, chained in ONE connected
    //     session (the second spends the first's change via the local
    //     mempool view, no re-sync). ---
    let chain_recipients = [recipient_address(20), recipient_address(21)];
    let chain_outputs: Vec<String> = chain_recipients
        .iter()
        .map(|address| format!("{address}:5"))
        .collect();
    let chain_args = Args {
        max_outputs_per_tx: 1,
        ..batch_send_args(&keys_file_path, chain_outputs)
    };
    let summary = run_with_client_and_params(chain_args, kaspad_client.clone(), params.clone())
        .await
        .expect("chained send failed");
    assert_eq!(
        summary.transaction_ids.len(),
        2,
        "one output per tx forces two chained transactions"
    );
    assert_eq!(summary.transaction_ids[0], summary.paid[0].tx_id);
    assert_ne!(
        summary.paid[0].tx_id, summary.paid[1].tx_id,
        "each recipient is paid by a distinct chained transaction"
    );
    assert_eq!(summary.paid.len(), 2);
    // A chained parent+child may confirm across successive blocks — mine a
    // few so both land in the utxoindex before asserting balances.
    for _ in 0..3 {
        mine_block(kaspad_client.clone(), NULL_ADDRESS).await;
        sleep(Duration::from_millis(500)).await;
    }
    for recipient in &chain_recipients {
        assert_eq!(
            recipient_amount_sompi(&kaspad_client, recipient).await,
            5 * SOMPI_PER_KASPA,
            "each chained recipient must hold exactly the sent amount"
        );
    }

    // --- Case 4: overdraft leaves everyone unpaid and submits nothing ---
    let overdraft_recipient = recipient_address(9);
    let overdraft_args = batch_send_args(
        &keys_file_path,
        vec![format!("{overdraft_recipient}:1000000")],
    );
    let summary = run_with_client_and_params(overdraft_args, kaspad_client.clone(), params)
        .await
        .expect("overdraft returns a summary, not an error");
    assert!(summary.paid.is_empty());
    assert_eq!(summary.unpaid.len(), 1);
    assert!(summary.transaction_ids.is_empty());
    assert!(
        summary.failure.is_some(),
        "overdraft must record a failure reason"
    );
    mine_block(kaspad_client.clone(), NULL_ADDRESS).await;
    sleep(Duration::from_millis(500)).await;
    assert_eq!(
        recipient_amount_sompi(&kaspad_client, &overdraft_recipient).await,
        0,
        "failed send must not submit anything"
    );

    kaspad_daemon.shutdown();
}
