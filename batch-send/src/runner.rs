use crate::args::Args;
use crate::outputs::{parse_outputs, parse_outputs_file};
use common::amount::kas_display;
use common::args::calculate_path;
use common::error_location::ErrorLocation;
use common::errors::{
    RpcError, StorageError, SyncError, TransactionError, UserInputError, WalletError, WalletResult,
};
use common::keys::Keys;
use common::model::{
    Keychain, WalletAddress, WalletPayment, WalletSignableTransaction, WalletSigned,
};
use common::status_classify::classify_submit_rpc_error;
use kaspa_bip32::Prefix as ExtendedKeyPrefix;
use kaspa_bip32::{ExtendedPrivateKey, SecretKey};
use kaspa_consensus_core::config::params::Params;
use kaspa_consensus_core::network::NetworkId;
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_grpc_client::GrpcClient;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_wallet_core::tx::MassCalculator;
use kaswallet_daemon::address_manager::AddressManager;
use kaswallet_daemon::kaspad_client;
use kaswallet_daemon::signer;
use kaswallet_daemon::sync_manager::SyncManager;
use kaswallet_daemon::transaction_generator::{
    TransactionGenerator, checked_payment_sum, storage_mass_for_signable,
};
use kaswallet_daemon::utxo_manager::UtxoManager;
use secrecy::SecretString;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// One recipient that was paid, and the transaction that paid it.
#[derive(Debug, Clone)]
pub struct PaidRecipient {
    pub address: String,
    pub amount_sompi: u64,
    /// Node-accepted tx id (or, under `--dry-run`, the locally-computed id).
    pub tx_id: String,
}

/// Outcome of a batch send. Recipients are paid across one or more chained
/// transactions (one connected session); on a mid-chain failure the paid
/// prefix is still reported so a caller can resume with only the remainder.
#[derive(Debug)]
pub struct SendSummary {
    /// Submitted (or, on `--dry-run`, built) transaction ids, in order.
    pub transaction_ids: Vec<String>,
    pub paid: Vec<PaidRecipient>,
    /// `(address, amount_sompi)` for recipients not paid (empty on success).
    pub unpaid: Vec<(String, u64)>,
    /// Total recipients requested.
    pub recipients: usize,
    pub requested_sompi: u64,
    /// Aggregate fee across all submitted transactions.
    pub fee_sompi: u64,
    pub dry_run: bool,
    /// Human reason + process exit code when distribution stopped early.
    pub failure: Option<(String, i32)>,
}

/// Everything that can be validated without touching the network: parsed
/// recipients, loaded keys, and password-verified private keys. Prepared
/// BEFORE any kaspad connection so a wrong password or malformed output
/// list fails fast (PRD: "password verified before any network work").
struct OfflinePreparation {
    payments: Vec<WalletPayment>,
    requested_total: u64,
    keys: Arc<Keys>,
    private_keys: Vec<ExtendedPrivateKey<SecretKey>>,
}

/// Resolve the network-specific keys file location and load the keys.
/// Also returns the resolved path so callers can log it.
fn load_keys(args: &Args, network_id: NetworkId) -> WalletResult<(String, Arc<Keys>)> {
    let keys_file_path = calculate_path(&args.keys_file_path, &network_id, "keys.json");
    let keys = Arc::new(Keys::load(
        &keys_file_path,
        ExtendedKeyPrefix::from(network_id),
    )?);
    Ok((keys_file_path, keys))
}

fn prepare_offline(args: &Args) -> WalletResult<OfflinePreparation> {
    let network_id = args.network_id();
    let address_prefix: kaspa_addresses::Prefix = network_id.network_type.into();

    let payments = match &args.outputs_file {
        Some(path) => parse_outputs_file(path, address_prefix)?,
        None => parse_outputs(&args.outputs, address_prefix)?,
    };
    let requested_total = checked_payment_sum(&payments)?;

    let (keys_file_path, keys) = load_keys(args, network_id)?;
    let wallet_kind = if keys.public_keys.len() > 1 {
        format!(
            "{}-of-{} multisig",
            keys.minimum_signatures,
            keys.public_keys.len()
        )
    } else {
        "single-sig".to_string()
    };
    info!(
        network = %network_id,
        keys_file = %keys_file_path,
        wallet = %wallet_kind,
        "starting batch send: {} recipient(s), {} KAS requested",
        payments.len(),
        kas_display(requested_total),
    );

    let password = get_password(args.password.clone())?;
    let private_keys = signer::decrypt_private_keys(&keys, &password)?;
    info!("wallet unlocked (password verified)");

    Ok(OfflinePreparation {
        payments,
        requested_total,
        keys,
        private_keys,
    })
}

/// Derive the wallet's receive address (external keychain, index 0 —
/// always inside the wallet's address-scan window) from the keys file.
/// Fully offline: only the public key is read, so no node connection and
/// no password are needed. Backs `--show-address`.
pub async fn show_receive_address(args: &Args) -> WalletResult<kaspa_addresses::Address> {
    let network_id = args.network_id();
    let address_prefix: kaspa_addresses::Prefix = network_id.network_type.into();
    let (_, keys) = load_keys(args, network_id)?;
    let address_manager = AddressManager::new(keys.clone(), address_prefix);
    address_manager
        .kaspa_address_from_wallet_address(
            &WalletAddress::new(0, keys.cosigner_index, Keychain::External),
            false,
        )
        .await
}

/// Connect to kaspad per `args` and derive the network's consensus params —
/// the two pieces the `_with_client_and_params` entry points take injected.
async fn connect_client_and_params(args: &Args) -> WalletResult<(Arc<GrpcClient>, Params)> {
    let network_id = args.network_id();
    let kaspa_client = Arc::new(kaspad_client::connect(&args.server, &network_id).await?);
    Ok((kaspa_client, Params::from(network_id.network_type)))
}

/// Connect to kaspad per `args` and fetch the wallet's balance (no
/// password needed — the scan uses public-key-derived addresses only).
/// Backs the balance half of `--show-address`.
pub async fn wallet_balance(args: &Args) -> WalletResult<WalletBalance> {
    let (kaspa_client, consensus_params) = connect_client_and_params(args).await?;
    wallet_balance_with_client_and_params(args, kaspa_client, consensus_params).await
}

/// The wallet-side manager graph — the same wiring as daemon startup,
/// minus the gRPC server and the background sync loop.
struct WalletComponents {
    address_manager: Arc<Mutex<AddressManager>>,
    utxo_manager: Arc<Mutex<UtxoManager>>,
    sync_manager: SyncManager,
}

fn build_wallet_components(
    kaspa_client: Arc<GrpcClient>,
    keys: Arc<Keys>,
    address_prefix: kaspa_addresses::Prefix,
    consensus_params: Params,
) -> WalletComponents {
    let address_manager = Arc::new(Mutex::new(AddressManager::new(
        keys.clone(),
        address_prefix,
    )));
    let utxo_manager = Arc::new(Mutex::new(UtxoManager::new(
        address_manager.clone(),
        consensus_params,
    )));
    // Interval is irrelevant here — the loop is never started, only sync_once.
    let sync_manager = SyncManager::new(
        kaspa_client,
        keys,
        address_manager.clone(),
        utxo_manager.clone(),
        10_000,
    );
    WalletComponents {
        address_manager,
        utxo_manager,
        sync_manager,
    }
}

/// Balance fetch against an injected client + params (test entry point,
/// same split as `run` / `run_with_client_and_params`).
pub async fn wallet_balance_with_client_and_params(
    args: &Args,
    kaspa_client: Arc<GrpcClient>,
    consensus_params: Params,
) -> WalletResult<WalletBalance> {
    let network_id = args.network_id();
    let address_prefix: kaspa_addresses::Prefix = network_id.network_type.into();
    let (_, keys) = load_keys(args, network_id)?;

    check_node(args, &kaspa_client).await?;

    let WalletComponents {
        address_manager,
        utxo_manager,
        sync_manager,
    } = build_wallet_components(kaspa_client.clone(), keys, address_prefix, consensus_params);
    info!("syncing wallet view (address scan + UTXO fetch)...");
    sync_manager.sync_once().await?;

    log_wallet_state(&kaspa_client, &utxo_manager, &address_manager).await
}

/// Connect to kaspad per `args` and run the batch send. Offline validation
/// (outputs, keys, password) runs BEFORE the connect — kaspa's
/// `GrpcClient::connect` is eager, so a down node must not mask a wrong
/// password or a bad recipient list.
pub async fn run(args: Args) -> WalletResult<SendSummary> {
    let preparation = prepare_offline(&args)?;
    let (kaspa_client, consensus_params) = connect_client_and_params(&args).await?;
    run_prepared(args, preparation, kaspa_client, consensus_params).await
}

/// The full flow against an injected kaspad client + consensus params
/// (integration tests inject a simnet client with patched params — same
/// split as `Daemon::start` / `start_with_kaspad_client_and_consensus_params`).
pub async fn run_with_client_and_params(
    args: Args,
    kaspa_client: Arc<GrpcClient>,
    consensus_params: Params,
) -> WalletResult<SendSummary> {
    let preparation = prepare_offline(&args)?;
    run_prepared(args, preparation, kaspa_client, consensus_params).await
}

async fn run_prepared(
    args: Args,
    preparation: OfflinePreparation,
    kaspa_client: Arc<GrpcClient>,
    consensus_params: Params,
) -> WalletResult<SendSummary> {
    let network_id = args.network_id();
    let address_prefix: kaspa_addresses::Prefix = network_id.network_type.into();
    let OfflinePreparation {
        payments,
        requested_total,
        keys,
        private_keys,
    } = preparation;
    let recipient_count = payments.len();

    // ---- Node sanity checks ----
    check_node(&args, &kaspa_client).await?;

    // ---- Wallet component graph + transaction generator ----
    let WalletComponents {
        address_manager,
        utxo_manager,
        sync_manager,
    } = build_wallet_components(
        kaspa_client.clone(),
        keys.clone(),
        address_prefix,
        consensus_params.clone(),
    );
    let mass_calculator = Arc::new(MassCalculator::new(&network_id.network_type.into()));
    let mut transaction_generator = TransactionGenerator::new(
        kaspa_client.clone(),
        keys.clone(),
        address_manager.clone(),
        mass_calculator,
        address_prefix,
        SUBNETWORK_ID_NATIVE,
        &consensus_params,
    )?;

    // Always surface where this wallet receives funds — confirms the right
    // wallet is in use, and tells the user where to send KAS when the
    // balance turns out to be empty.
    {
        let address_manager_guard = address_manager.lock().await;
        let receive_address = address_manager_guard
            .kaspa_address_from_wallet_address(
                &WalletAddress::new(0, keys.cosigner_index, Keychain::External),
                true,
            )
            .await?;
        info!("wallet receive address (external index 0): {receive_address}");
    }

    info!("syncing wallet view (address scan + UTXO fetch)...");
    sync_manager.sync_once().await?;

    // ---- Wallet state + explicit balance check ----
    let spendable_sompi = log_wallet_state(&kaspa_client, &utxo_manager, &address_manager)
        .await?
        .spendable_sompi;
    info!(
        "balance check: requested {} KAS to {} recipient(s); spendable {} KAS (fees come on top)",
        kas_display(requested_total),
        recipient_count,
        kas_display(spendable_sompi),
    );
    if requested_total > spendable_sompi {
        warn!(
            "requested amount exceeds spendable balance — selection will fail (short by {} KAS before fees)",
            kas_display(requested_total - spendable_sompi),
        );
    }

    // ---- Distribute across one or more chained transactions ----
    distribute(
        &args,
        &mut transaction_generator,
        &utxo_manager,
        &address_manager,
        &consensus_params,
        &private_keys,
        &kaspa_client,
        payments,
    )
    .await
}

fn is_mass_exceeded(error: &WalletError) -> bool {
    matches!(
        error,
        WalletError::Transaction(TransactionError::MassExceeded { .. })
    )
}

/// Pay every recipient across one connected session, packing as many outputs
/// per transaction as the KIP-9 storage-mass limit allows and chaining the
/// rest (each transaction spends the previous one's change, registered in the
/// local UTXO view — no re-sync or reconnection between transactions).
#[allow(clippy::too_many_arguments)]
async fn distribute(
    args: &Args,
    transaction_generator: &mut TransactionGenerator,
    utxo_manager: &Arc<Mutex<UtxoManager>>,
    address_manager: &Arc<Mutex<AddressManager>>,
    consensus_params: &Params,
    private_keys: &[ExtendedPrivateKey<SecretKey>],
    kaspa_client: &Arc<GrpcClient>,
    payments: Vec<WalletPayment>,
) -> WalletResult<SendSummary> {
    let recipients = payments.len();
    let requested_sompi = checked_payment_sum(&payments)?;
    let max_per_tx = args.max_outputs_per_tx.max(1) as usize;

    let mut summary = SendSummary {
        transaction_ids: vec![],
        paid: vec![],
        unpaid: vec![],
        recipients,
        requested_sompi,
        fee_sompi: 0,
        dry_run: args.dry_run,
        failure: None,
    };

    // Mark payments[from..] unpaid and record why distribution stopped.
    let stop = |summary: &mut SendSummary, from: usize, error: WalletError| {
        summary.failure = Some((error.user_message(), error.process_exit_code()));
        for payment in &payments[from..] {
            summary
                .unpaid
                .push((payment.address.to_string(), payment.amount));
        }
    };

    let mut index = 0usize;
    // Seed each chunk's size from the previous success so equal-amount runs
    // converge to the max fit in ~O(N) builds instead of re-probing.
    let mut seed = max_per_tx;
    while index < payments.len() {
        let remaining = payments.len() - index;
        let mut attempt = seed.min(max_per_tx).min(remaining).max(1);

        let (unsigned, used) = loop {
            let chunk: Vec<WalletPayment> = payments[index..index + attempt].to_vec();
            let build = {
                let guard = utxo_manager.lock().await;
                transaction_generator
                    .create_unsigned_transactions_for_payments(
                        &guard,
                        chunk,
                        args.fee_policy(),
                        args.use_existing_change_address,
                    )
                    .await
            };
            match build {
                Ok(mut txs) => break (txs.remove(0), attempt),
                Err(error) if is_mass_exceeded(&error) && attempt > 1 => {
                    // Too many outputs for one transaction — pack fewer.
                    attempt /= 2;
                }
                Err(error) => {
                    // attempt == 1 MassExceeded (single output is dust) or any
                    // other failure (insufficient funds, RPC): stop here.
                    warn!(
                        "distribution stopped at recipient {}: {}",
                        index + 1,
                        error.user_message()
                    );
                    stop(&mut summary, index, error);
                    return Ok(summary);
                }
            }
        };

        let fee_sompi =
            log_transaction_plan(&unsigned, used, address_manager, consensus_params).await?;

        // ---- Sign ----
        let signed = match sign_fully(private_keys, unsigned) {
            Ok(signed) => signed,
            Err(error) => {
                stop(&mut summary, index, error);
                return Ok(summary);
            }
        };
        let tx_id = signed.transaction.inner().tx.id().to_string();

        // ---- Submit (skipped on --dry-run) ----
        if args.dry_run {
            info!(tx_id = %tx_id, "DRY RUN — transaction built and chained locally, not submitted");
        } else {
            let rpc_transaction = (&signed.transaction.inner().tx).into();
            if let Err(rpc_err) = kaspa_client
                .submit_transaction(rpc_transaction, false)
                .await
            {
                let error = WalletError::from(classify_submit_rpc_error(
                    signed.transaction.inner().tx.id(),
                    rpc_err,
                ));
                stop(&mut summary, index, error);
                return Ok(summary);
            }
            info!(tx_id = %tx_id, "transaction submitted");
        }

        // Register into the local UTXO view (removes spent inputs, adds this
        // transaction's change as spendable) so the next chunk chains without
        // an RPC. Done for dry-run too, so a rehearsal exercises the full
        // chain locally.
        {
            let mut guard = utxo_manager.lock().await;
            guard.add_mempool_transaction(&signed).await;
        }

        for offset in 0..used {
            let payment = &payments[index + offset];
            summary.paid.push(PaidRecipient {
                address: payment.address.to_string(),
                amount_sompi: payment.amount,
                tx_id: tx_id.clone(),
            });
        }
        summary.transaction_ids.push(tx_id);
        summary.fee_sompi = summary.fee_sompi.saturating_add(fee_sompi);
        seed = used;
        index += used;
    }

    Ok(summary)
}

/// Sign a single built transaction and return it, requiring a fully-signed
/// result (multisig cosigning is unsupported).
fn sign_fully(
    private_keys: &[ExtendedPrivateKey<SecretKey>],
    unsigned: WalletSignableTransaction,
) -> WalletResult<WalletSignableTransaction> {
    let mut signed = signer::sign_transactions_with_keys(private_keys, vec![unsigned])?;
    let signed = signed.remove(0);
    match &signed.transaction {
        WalletSigned::Fully(_) => Ok(signed),
        WalletSigned::Partially(_) => {
            warn!(
                "keys file cannot fully sign this transaction (multisig cosigning is not \
                 supported by kaswallet-batch-send); nothing submitted"
            );
            Err(WalletError::from(TransactionError::NotFullySigned {
                location: ErrorLocation::capture(),
            }))
        }
    }
}

/// Verify the connected kaspad is usable for wallet operations: right
/// network, UTXO index enabled, and synced (unless explicitly allowed).
async fn check_node(args: &Args, kaspa_client: &Arc<GrpcClient>) -> WalletResult<()> {
    let network_id = args.network_id();
    let server_info = kaspa_client
        .get_server_info()
        .await
        .map_err(|e| RpcError::Transport {
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })?;
    info!(
        server_version = %server_info.server_version,
        node_network = %server_info.network_id,
        is_synced = server_info.is_synced,
        has_utxo_index = server_info.has_utxo_index,
        virtual_daa_score = server_info.virtual_daa_score,
        "connected to kaspad"
    );
    if server_info.network_id != network_id {
        return Err(WalletError::from(UserInputError::InvalidArgument {
            reason: format!(
                "kaspad network mismatch: node is on {}, tool started for {}",
                server_info.network_id, network_id
            ),
            location: ErrorLocation::capture(),
        }));
    }
    if !server_info.has_utxo_index {
        return Err(WalletError::from(UserInputError::InvalidArgument {
            reason: "kaspad runs without a UTXO index; restart it with --utxoindex".to_string(),
            location: ErrorLocation::capture(),
        }));
    }
    if !server_info.is_synced {
        if args.allow_unsynced_node {
            warn!("kaspad reports it is not synced — proceeding due to --allow-unsynced-node");
        } else {
            return Err(WalletError::from(SyncError::NodeNotSynced {
                location: ErrorLocation::capture(),
            }));
        }
    }
    Ok(())
}

fn get_password(password_arg: Option<String>) -> WalletResult<SecretString> {
    let raw = if let Some(password) = password_arg {
        password
    } else {
        // Prompt on stderr — stdout is reserved for the parseable summary.
        eprint!("Password: ");
        std::io::stderr().flush().map_err(|e| {
            WalletError::from(StorageError::Io {
                path: "stderr".into(),
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })
        })?;
        rpassword::read_password().map_err(|e| {
            WalletError::from(StorageError::Io {
                path: "stdin".into(),
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })
        })?
    };
    Ok(SecretString::from(raw))
}

/// Wallet balance snapshot (sompi). `spendable` excludes immature coinbase
/// and unconfirmed UTXOs; `total` includes them.
#[derive(Debug)]
pub struct WalletBalance {
    pub spendable_sompi: u64,
    pub total_sompi: u64,
}

/// Fold one spendable UTXO / transaction input into a per-address
/// `(sompi, count)` aggregate keyed by bech32 address string.
fn accumulate_per_address(
    per_address: &mut BTreeMap<String, (u64, usize)>,
    address: String,
    amount: u64,
) {
    let entry = per_address.entry(address).or_insert((0, 0));
    entry.0 = entry.0.saturating_add(amount);
    entry.1 += 1;
}

/// Log the post-sync wallet state (spendable vs total balance, funded
/// addresses) and return the balance snapshot.
async fn log_wallet_state(
    kaspa_client: &Arc<GrpcClient>,
    utxo_manager: &Arc<Mutex<UtxoManager>>,
    address_manager: &Arc<Mutex<AddressManager>>,
) -> WalletResult<WalletBalance> {
    let dag_info = kaspa_client
        .get_block_dag_info()
        .await
        .map_err(|e| RpcError::Transport {
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })?;

    let utxo_manager_guard = utxo_manager.lock().await;
    let address_manager_guard = address_manager.lock().await;
    let utxos = utxo_manager_guard.utxos_by_outpoint();

    let mut total_sompi: u64 = 0;
    let mut spendable_sompi: u64 = 0;
    let mut spendable_utxo_count: usize = 0;
    // (spendable sompi, utxo count) per bech32 source address.
    let mut per_address: BTreeMap<String, (u64, usize)> = BTreeMap::new();
    for utxo in utxos.values() {
        let amount = utxo.utxo_entry.amount;
        total_sompi = total_sompi.saturating_add(amount);
        if utxo_manager_guard.is_utxo_unspendable(utxo, dag_info.virtual_daa_score) {
            continue;
        }
        spendable_sompi = spendable_sompi.saturating_add(amount);
        spendable_utxo_count += 1;
        let address = address_manager_guard
            .kaspa_address_from_wallet_address(&utxo.address, true)
            .await?;
        accumulate_per_address(&mut per_address, address.to_string(), amount);
    }

    info!(
        "wallet state: spendable {} KAS in {} UTXO(s) across {} address(es); total (incl. immature/unconfirmed) {} KAS in {} UTXO(s)",
        kas_display(spendable_sompi),
        spendable_utxo_count,
        per_address.len(),
        kas_display(total_sompi),
        utxos.len(),
    );

    const MAX_ADDRESSES_AT_INFO: usize = 10;
    let mut by_amount: Vec<(&String, &(u64, usize))> = per_address.iter().collect();
    by_amount.sort_by_key(|entry| std::cmp::Reverse(entry.1.0));
    for (address, (amount, count)) in by_amount.iter().take(MAX_ADDRESSES_AT_INFO) {
        info!(
            "  funded address {}: {} KAS ({} UTXO(s))",
            address,
            kas_display(*amount),
            count
        );
    }
    if by_amount.len() > MAX_ADDRESSES_AT_INFO {
        info!(
            "  ... and {} more funded address(es) (run with -v for the full list)",
            by_amount.len() - MAX_ADDRESSES_AT_INFO
        );
        for (address, (amount, count)) in by_amount.iter().skip(MAX_ADDRESSES_AT_INFO) {
            debug!(
                "  funded address {}: {} KAS ({} UTXO(s))",
                address,
                kas_display(*amount),
                count
            );
        }
    }

    Ok(WalletBalance {
        spendable_sompi,
        total_sompi,
    })
}

/// Log the built transaction: sending (source) addresses with per-address
/// contributions, per-recipient outputs, change, fee and masses. Returns the
/// fee in sompi.
async fn log_transaction_plan(
    transaction: &WalletSignableTransaction,
    recipient_count: usize,
    address_manager: &Arc<Mutex<AddressManager>>,
    consensus_params: &Params,
) -> WalletResult<u64> {
    let signable = transaction.transaction.inner();
    let total_in: u64 = signable
        .entries
        .iter()
        .flatten()
        .map(|entry| entry.amount)
        .sum();
    let total_out: u64 = signable.tx.outputs.iter().map(|output| output.value).sum();
    let fee_sompi = total_in.saturating_sub(total_out);

    // Sending (source) addresses, aggregated per address.
    let address_manager_guard = address_manager.lock().await;
    let mut sources: BTreeMap<String, (u64, usize)> = BTreeMap::new();
    for (input_index, wallet_address) in transaction.address_by_input_index.iter().enumerate() {
        let amount = signable
            .entries
            .get(input_index)
            .and_then(|entry| entry.as_ref())
            .map(|entry| entry.amount)
            .unwrap_or(0);
        let address = address_manager_guard
            .kaspa_address_from_wallet_address(wallet_address, true)
            .await?;
        accumulate_per_address(&mut sources, address.to_string(), amount);
    }
    drop(address_manager_guard);

    let change_sompi = if signable.tx.outputs.len() > recipient_count {
        signable.tx.outputs[signable.tx.outputs.len() - 1].value
    } else {
        0
    };

    let masses = signable.calculated_non_contextual_masses;
    let storage_mass = storage_mass_for_signable(signable, consensus_params.storage_mass_parameter);
    info!(
        tx_id = %signable.tx.id(),
        inputs = signable.tx.inputs.len(),
        outputs = signable.tx.outputs.len(),
        compute_mass = masses.map(|m| m.compute_mass),
        transient_mass = masses.map(|m| m.transient_mass),
        storage_mass,
        "transaction built: sending {} KAS to {} recipient(s), fee {} KAS, change {} KAS",
        kas_display(total_out.saturating_sub(change_sompi)),
        recipient_count,
        kas_display(fee_sompi),
        kas_display(change_sompi),
    );
    if let Some(masses) = masses
        && masses.compute_mass > 0
    {
        debug!(
            "effective fee rate ≈ {:.2} sompi per compute gram",
            fee_sompi as f64 / masses.compute_mass as f64
        );
    }

    for (address, (amount, count)) in &sources {
        info!(
            "  sending from {}: {} KAS ({} UTXO(s))",
            address,
            kas_display(*amount),
            count
        );
    }
    for (output_index, output) in signable.tx.outputs.iter().enumerate() {
        let address = &transaction.address_by_output_index[output_index];
        let is_change = output_index >= recipient_count;
        info!(
            "  {} <- {} KAS{}",
            address,
            kas_display(output.value),
            if is_change { " (change)" } else { "" }
        );
    }

    Ok(fee_sompi)
}
