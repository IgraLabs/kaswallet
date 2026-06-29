use crate::address_manager::AddressManager;
use crate::utxo_manager::UtxoManager;
use common::error_location::ErrorLocation;
use common::errors::{TransactionError, UserInputError as UserInputErr, WalletError, WalletResult};
use common::keys::Keys;
use common::model::{
    WalletAddress, WalletOutpoint, WalletPayment, WalletSignableTransaction, WalletUtxo,
    WalletUtxoEntry,
};
use itertools::Itertools;
use kaspa_addresses::{Address, Version};
use kaspa_bip32::DerivationPath;
use kaspa_consensus_core::config::params::Params;
use kaspa_consensus_core::constants::{
    SOMPI_PER_KASPA, TRANSIENT_BYTE_TO_MASS_FACTOR, TX_VERSION, TX_VERSION_TOCCATA,
    UNACCEPTED_DAA_SCORE,
};
use kaspa_consensus_core::mass::{
    GRAMS_PER_COMPUTE_BUDGET_UNIT, MassCofactors, NonContextualMasses,
    transaction_estimated_serialized_size,
};
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    ComputeCommit, SignableTransaction, Transaction, TransactionInput, TransactionOutpoint,
    TransactionOutput, UtxoEntry,
};
use kaspa_grpc_client::GrpcClient;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_txscript::pay_to_address_script;
use kaspa_wallet_core::prelude::AddressPrefix;
use kaspa_wallet_core::tx::{MAXIMUM_STANDARD_TRANSACTION_MASS, MassCalculator, SIGNATURE_SIZE};
use proto::kaswallet_proto::{FeePolicy, Outpoint, TransactionDescription, fee_policy};
use std::cmp::min;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};
use tracing::{debug, info};

// The current minimal fee rate according to mempool standards
const MIN_FEE_RATE: f64 = 1.0;

// The minimal change amount to target in order to avoid large storage mass (see KIP9 for more details).
// By having at least 10KAS in the change output we make sure that the storage mass charged for change is
// at most 1000 gram. Generally, if the payment is above 10KAS as well, the resulting storage mass will be
// in the order of magnitude of compute mass and will not incur additional charges.
// Additionally, every transaction with send value > ~0.1 KAS should succeed (at most ~99K storage mass for payment
// output, thus overall lower than standard mass upper bound which is 100K gram)
const MIN_CHANGE_TARGET: u64 = SOMPI_PER_KASPA * 10;

/// Pick the consensus transaction version for a given subnetwork.
///
/// Native subnetwork uses `TX_VERSION` (0); any other subnetwork carries
/// the Toccata-era v1 version that enables non-native subnetwork
/// transactions on Toccata-active networks.
pub(crate) fn select_tx_version(subnetwork_id: &SubnetworkId) -> u16 {
    if subnetwork_id.is_native() {
        TX_VERSION
    } else {
        TX_VERSION_TOCCATA
    }
}

/// Compute the per-input compute budget for a v1 transaction.
///
/// Mirrors the pattern used by upstream consensus tests:
/// `compute_budget = (mass_per_sig_op / GRAMS_PER_COMPUTE_BUDGET_UNIT) * minimum_signatures`.
///
/// Returns an error if the result overflows `u16` (the consensus-side type)
/// — fail loudly rather than saturating silently.
fn compute_budget_for_signature(
    mass_per_sig_op: u64,
    minimum_signatures: u16,
) -> WalletResult<u16> {
    // Use `div_ceil` so any `mass_per_sig_op` that is not a clean multiple
    // of `GRAMS_PER_COMPUTE_BUDGET_UNIT` rounds up — under-budgeting by
    // one unit on a network that ships a non-canonical mass-per-sig-op
    // produces v1 transactions consensus rejects as "compute exceeded".
    let per_sig =
        u16::try_from(mass_per_sig_op.div_ceil(GRAMS_PER_COMPUTE_BUDGET_UNIT)).map_err(|_| {
            WalletError::from(UserInputErr::InvalidArgument {
                reason: format!(
                    "mass_per_sig_op/GRAMS_PER_COMPUTE_BUDGET_UNIT must fit u16 \
                     (mass_per_sig_op={mass_per_sig_op})"
                ),
                location: ErrorLocation::capture(),
            })
        })?;
    per_sig.checked_mul(minimum_signatures).ok_or_else(|| {
        WalletError::from(UserInputErr::InvalidArgument {
            reason: format!(
                "compute_budget overflow: per-sig {per_sig} * minimum_signatures \
                 {minimum_signatures} exceeds u16::MAX"
            ),
            location: ErrorLocation::capture(),
        })
    })
}

/// Estimated transient mass of the *signed* transaction = serialized_size · TRANSIENT_BYTE_TO_MASS_FACTOR.
/// The unsigned mock has empty signature scripts, so the signed signature footprint (`SIGNATURE_SIZE`
/// per required signature per input) is added before scaling — the node charges transient on the signed
/// serialized size, mirroring the compute side's signature accounting. Free function (no generator state)
/// so it is unit-testable.
fn estimate_transient_mass(tx: &Transaction, minimum_signatures: u16) -> u64 {
    let signature_bytes =
        SIGNATURE_SIZE * (minimum_signatures.max(1) as u64) * (tx.inputs.len() as u64);
    let signed_serialized_size = transaction_estimated_serialized_size(tx) + signature_bytes;
    signed_serialized_size * TRANSIENT_BYTE_TO_MASS_FACTOR
}

pub struct TransactionGenerator {
    kaspa_client: Arc<GrpcClient>,
    keys: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    mass_calculator: Arc<MassCalculator>,
    address_prefix: AddressPrefix,
    subnetwork_id: SubnetworkId,
    tx_version: u16,
    compute_budget_per_input: u16,
    /// `keys.minimum_signatures` narrowed to `u8` once at construction so the
    /// v0 input builder never silently truncates a wider value.
    minimum_signatures_u8: u8,
    /// Authoritative `mass_per_sig_op` from `ConsensusParams`. Used by
    /// `estimate_mass_per_input` for the v0 sig-op contribution so the local
    /// approximation tracks whatever the actual network ships with.
    mass_per_sig_op: u64,

    signature_mass_per_input: u64,

    /// Post-Toccata mempool mass cofactors (transient cofactor = compute_block_limit /
    /// transient_block_limit). Used to normalize transient mass to the compute scale exactly as the
    /// node's standardness relay-fee floor does — the node reads `mempool_mass_cofactors.raw_post()`.
    mass_cofactors: MassCofactors,
}

impl TransactionGenerator {
    pub fn new(
        kaspa_client: Arc<GrpcClient>,
        keys: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        mass_calculator: Arc<MassCalculator>,
        address_prefix: AddressPrefix,
        subnetwork_id: SubnetworkId,
        consensus_params: &Params,
    ) -> WalletResult<Self> {
        if keys.minimum_signatures == 0 {
            return Err(WalletError::from(UserInputErr::InvalidArgument {
                reason: "keys.minimum_signatures must be at least 1".to_string(),
                location: ErrorLocation::capture(),
            }));
        }
        let minimum_signatures_u8 = u8::try_from(keys.minimum_signatures).map_err(|_| {
            WalletError::from(UserInputErr::InvalidArgument {
                reason: format!(
                    "keys.minimum_signatures ({}) must fit in u8",
                    keys.minimum_signatures
                ),
                location: ErrorLocation::capture(),
            })
        })?;
        // Upstream made the per-input variant `pub(crate)` on Toccata; use the
        // public batch helper and request mass for a single input.
        let signature_mass_per_input =
            mass_calculator.calc_signature_compute_mass_for_inputs(1, keys.minimum_signatures);
        let tx_version = select_tx_version(&subnetwork_id);
        let compute_budget_per_input = compute_budget_for_signature(
            consensus_params.mass_per_sig_op,
            keys.minimum_signatures,
        )?;
        // One-time log of the resolved lane/version profile so an operator
        // can audit what shape this daemon will emit without having to
        // wait for the first tx.
        debug!(
            subnetwork_id = %subnetwork_id,
            tx_version,
            compute_budget_per_input,
            mass_per_sig_op = consensus_params.mass_per_sig_op,
            minimum_signatures = keys.minimum_signatures,
            "transaction generator configured"
        );
        Ok(Self {
            kaspa_client,
            keys,
            address_manager,
            mass_calculator,
            address_prefix,
            subnetwork_id,
            tx_version,
            compute_budget_per_input,
            minimum_signatures_u8,
            mass_per_sig_op: consensus_params.mass_per_sig_op,
            signature_mass_per_input,
            mass_cofactors: consensus_params.mempool_block_mass_cofactors().raw_post(),
        })
    }

    pub async fn create_unsigned_transactions(
        &mut self,
        utxo_manager: &MutexGuard<'_, UtxoManager>,
        transaction_description: TransactionDescription,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        let validate_address = |address_string: String, _name: &str| -> WalletResult<Address> {
            match Address::try_from(address_string.clone()) {
                Ok(address) => Ok(address),
                Err(e) => Err(WalletError::from(UserInputErr::InvalidAddress {
                    input: address_string,
                    reason: e.to_string(),
                    location: ErrorLocation::capture(),
                })),
            }
        };

        let to_address = validate_address(transaction_description.to_address, "to")?;
        let address_set: HashMap<String, WalletAddress>;
        {
            let address_manager = self.address_manager.lock().await;
            address_set = address_manager.address_set().await;
        }

        if !transaction_description.from_addresses.is_empty()
            && !transaction_description.utxos.is_empty()
        {
            return Err(WalletError::from(TransactionError::BuildFailed {
                reason: "Cannot specify both from_addresses and utxos".to_string(),
                location: ErrorLocation::capture(),
            }));
        }

        let from_addresses = if transaction_description.from_addresses.is_empty() {
            vec![]
        } else {
            let mut from_addresses = vec![];
            for address_string in transaction_description.from_addresses {
                let wallet_address = address_set.get(&address_string).ok_or_else(|| {
                    WalletError::from(UserInputErr::InvalidAddress {
                        input: address_string.clone(),
                        reason: "From address is not in address set".to_string(),
                        location: ErrorLocation::capture(),
                    })
                })?;
                from_addresses.push(wallet_address);
            }
            from_addresses
        };
        let preselected_utxos = if transaction_description.utxos.is_empty() {
            HashMap::new()
        } else {
            let mut preselected_utxos = HashMap::new();
            let utxos_by_outpoint = utxo_manager.utxos_by_outpoint();
            for preselected_outpoint in &transaction_description.utxos {
                let wo: WalletOutpoint = preselected_outpoint.clone().try_into()?;
                if let Some(utxo) = utxos_by_outpoint.get(&wo) {
                    preselected_utxos.insert(utxo.outpoint.clone(), utxo.clone());
                } else {
                    let op = TransactionOutpoint::new(wo.transaction_id, wo.index);
                    return Err(WalletError::from(TransactionError::UtxoNotFound {
                        outpoint: op,
                        location: ErrorLocation::capture(),
                    }));
                }
            }
            preselected_utxos
        };

        let (fee_rate, max_fee) = self
            .calculate_fee_limits(transaction_description.fee_policy)
            .await?;

        let change_address: Address;
        let change_wallet_address: WalletAddress;
        {
            let address_manager = self.address_manager.lock().await;
            (change_address, change_wallet_address) = // TODO: check if I really need both.
                address_manager.change_address(transaction_description.use_existing_change_address, &from_addresses).await?;
        }

        let selected_utxos: Vec<WalletUtxo>;
        let amount_sent_to_recipient: u64;
        let change_sompi: u64;
        (selected_utxos, amount_sent_to_recipient, change_sompi) = self
            .select_utxos(
                utxo_manager,
                &preselected_utxos,
                transaction_description.amount,
                transaction_description.is_send_all,
                fee_rate,
                max_fee,
                &from_addresses,
                &transaction_description.payload,
            )
            .await?;

        // Enriched selection log: include each UTXO's block_daa_score,
        // is_unconfirmed, and is_coinbase. With these in the trail, an
        // Orphan rejection downstream can be matched back to the exact
        // input it came from — if block_daa_score > 0 the parent was
        // in kaspad's consensus UTXO set at sync time, pointing at a
        // reorg; block_daa_score == 0 with is_unconfirmed=true would
        // mean a mempool leak slipped past the filter.
        debug!(
            "Selected utxos: {}",
            selected_utxos
                .iter()
                .map(|u| format!(
                    "{}(amount={}, block_daa_score={}, is_unconfirmed={}, is_coinbase={})",
                    u.outpoint,
                    u.utxo_entry.amount,
                    u.utxo_entry.block_daa_score,
                    u.utxo_entry.is_unconfirmed,
                    u.utxo_entry.is_coinbase,
                ))
                .join(", ")
        );

        let mut payments = vec![WalletPayment::new(
            to_address.clone(),
            amount_sent_to_recipient,
        )];
        if change_sompi > 0 {
            payments.push(WalletPayment::new(change_address.clone(), change_sompi));
        }
        let unsigned_transaction = self
            .generate_unsigned_transaction(
                payments,
                &selected_utxos,
                transaction_description.payload.into(),
            )
            .await?;

        let unsigned_transactions = self
            .maybe_auto_compound_transaction(
                utxo_manager,
                unsigned_transaction,
                &selected_utxos,
                from_addresses,
                &to_address,
                transaction_description.amount,
                transaction_description.is_send_all,
                &transaction_description.utxos,
                &change_address,
                &change_wallet_address,
                fee_rate,
                max_fee,
            )
            .await?;

        Ok(unsigned_transactions)
    }

    #[allow(clippy::too_many_arguments)]
    async fn maybe_auto_compound_transaction(
        &self,
        utxo_manager: &MutexGuard<'_, UtxoManager>,
        original_wallet_transaction: WalletSignableTransaction,
        original_selected_utxos: &Vec<WalletUtxo>,
        from_addresses: Vec<&WalletAddress>,
        to_address: &Address,
        amount: u64,
        is_send_all: bool,
        preselected_utxo_outpoints: &Vec<Outpoint>,
        change_address: &Address,
        change_wallet_address: &WalletAddress,
        fee_rate: f64,
        max_fee: u64,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        self.check_transaction_fee_rate(&original_wallet_transaction, max_fee)?;

        let original_consensus_transaction = original_wallet_transaction.transaction.inner();

        let transaction_mass = self.compute_mass_for_unsigned_consensus_transaction(
            &original_consensus_transaction.tx,
            self.keys.minimum_signatures,
        );

        if transaction_mass < MAXIMUM_STANDARD_TRANSACTION_MASS {
            debug!("No need to auto-compound transaction");
            return Ok(vec![original_wallet_transaction]);
        }

        let (split_count, input_per_split_count) = self
            .split_and_input_per_split_counts(
                &original_wallet_transaction,
                original_consensus_transaction,
                transaction_mass,
                change_address,
                fee_rate,
                max_fee,
            )
            .await?;

        let mut split_transactions = vec![];
        for i in 0..split_count {
            let start_index = i * input_per_split_count;
            let end_index = start_index + input_per_split_count;

            let split_transaction = self
                .create_split_transaction(
                    &original_wallet_transaction,
                    original_consensus_transaction,
                    change_address,
                    start_index,
                    end_index,
                    fee_rate,
                    max_fee,
                )
                .await?;

            self.check_transaction_fee_rate(&split_transaction, max_fee)?;

            split_transactions.push(split_transaction);
        }
        debug!(
            "Transaction split into {} transactions",
            split_transactions.len()
        );

        let merge_transaction = self
            .merge_transaction(
                utxo_manager,
                &split_transactions,
                &original_consensus_transaction.tx,
                original_selected_utxos,
                &from_addresses,
                to_address,
                amount,
                is_send_all,
                preselected_utxo_outpoints,
                change_address,
                change_wallet_address,
                fee_rate,
                max_fee,
            )
            .await?;

        // Recursion will be 2-3 iterations deep even in the rarest cases, so considered safe...
        let split_merge_transaction = Box::pin(self.maybe_auto_compound_transaction(
            utxo_manager,
            merge_transaction,
            original_selected_utxos,
            from_addresses,
            to_address,
            amount,
            is_send_all,
            preselected_utxo_outpoints,
            change_address,
            change_wallet_address,
            fee_rate,
            max_fee,
        ))
        .await?;

        let all_transactions = [split_transactions, split_merge_transaction]
            .concat()
            .to_vec();

        Ok(all_transactions)
    }

    #[allow(clippy::too_many_arguments)]
    async fn merge_transaction(
        &self,
        utxo_manager: &MutexGuard<'_, UtxoManager>,
        split_transactions: &[WalletSignableTransaction],
        original_consensus_transaction: &Transaction,
        original_selected_utxos: &[WalletUtxo],
        from_addresses: &[&WalletAddress],
        to_address: &Address,
        amount: u64,
        is_send_all: bool,
        preselected_utxo_outpoints: &[Outpoint],
        change_address: &Address,
        change_wallet_address: &WalletAddress,
        fee_rate: f64,
        max_fee: u64,
    ) -> WalletResult<WalletSignableTransaction> {
        let num_outputs = original_consensus_transaction.outputs.len();
        if ![1, 2].contains(&num_outputs) {
            // This is a sanity check to make sure originalTransaction has either 1 or 2 outputs:
            // 1. For the payment itself
            // 2. (optional) for change
            return Err(WalletError::from(TransactionError::BuildFailed {
                reason: format!(
                    "Original transaction has {} outputs, while 1 or 2 are expected",
                    num_outputs
                ),
                location: ErrorLocation::capture(),
            }));
        }

        let mut total_value = 0u64;
        let mut utxos_from_split_transactions = vec![];

        for split_transaction in split_transactions {
            let split_consensus_transaction = split_transaction.transaction.inner();
            let split_consensus_transaction = &split_consensus_transaction.tx;
            let output = &split_consensus_transaction.outputs[0];
            let utxo = WalletUtxo {
                outpoint: WalletOutpoint {
                    transaction_id: split_transaction.transaction.inner().id(),
                    index: 0,
                },
                utxo_entry: WalletUtxoEntry {
                    amount: output.value,
                    script_public_key: output.script_public_key.clone(),
                    block_daa_score: UNACCEPTED_DAA_SCORE,
                    is_coinbase: false,
                    // Synthetic UTXO representing the output of a split tx
                    // we are about to submit ourselves in the same Send
                    // batch (`SubmitSource::Internal`). The merge tx that
                    // will consume this is broadcast sequentially after
                    // its parent in `submit_transactions`, and the wallet
                    // tracks parents via `mempool_transactions` + replay,
                    // so chaining here is safe by design — same rationale
                    // as `apply_mempool_transaction` in `utxo_manager.rs`.
                    is_unconfirmed: false,
                },
                address: change_wallet_address.clone(),
            };
            utxos_from_split_transactions.push(utxo);
            total_value += output.value;
        }

        // We're overestimating a bit by assuming that any transaction will have a change output
        let merge_transaction_fee = self
            .estimate_fee(
                &utxos_from_split_transactions,
                fee_rate,
                max_fee,
                amount,
                &original_consensus_transaction.payload,
            )
            .await?;
        debug!("merge_transaction_fee: {}", merge_transaction_fee);

        let mut available_value = total_value - merge_transaction_fee;
        debug!("available_value: {}", available_value);

        let mut sent_value = if !is_send_all {
            amount
        } else {
            let total_value_from_split_transactions: u64 = utxos_from_split_transactions
                .iter()
                .map(|utxo| utxo.utxo_entry.amount)
                .sum();
            debug!(
                "total_value_from_split_transactions: {}",
                total_value_from_split_transactions
            );

            total_value_from_split_transactions - merge_transaction_fee
        };
        let additional_utxos = if available_value < sent_value {
            let required_amount = sent_value - available_value;
            if is_send_all {
                debug!(
                    "Reducing sent value by {} to accommodate for merge transaction fee",
                    required_amount
                );
                available_value -= required_amount;
                sent_value -= required_amount;
                vec![]
            } else if !preselected_utxo_outpoints.is_empty() {
                return Err(WalletError::from(TransactionError::InsufficientFunds {
                    required_sompi: sent_value,
                    available_sompi: available_value,
                    location: ErrorLocation::capture(),
                }));
            } else {
                debug!(
                    "Adding more UTXOs to the merge transaction to cover fee; required amount: {}",
                    required_amount
                );
                // Sometimes the fees from compound transactions make the total output higher than what's
                // available from selected utxos, in such cases - find one more UTXO and use it.
                let (additional_utxos, total_value_added) = self
                    .more_utxos_for_merge_transaction(
                        utxo_manager,
                        original_consensus_transaction,
                        original_selected_utxos,
                        from_addresses,
                        required_amount,
                        fee_rate,
                    )
                    .await?;

                debug!(
                    "Adding {} UTXOs to the merge transaction with total_value_added: {}",
                    additional_utxos.len(),
                    total_value_added
                );
                additional_utxos
            }
        } else {
            vec![]
        };
        let utxos_for_merge_transactions =
            [utxos_from_split_transactions, additional_utxos].concat();

        let mut payments = vec![WalletPayment {
            address: to_address.clone(),
            amount: sent_value,
        }];

        if available_value > sent_value {
            payments.push(WalletPayment {
                address: change_address.clone(),
                amount: available_value - sent_value,
            });
        }
        debug!(
            "Creating merge transaction with {} payments",
            payments.len()
        );

        self.generate_unsigned_transaction(
            payments,
            &utxos_for_merge_transactions,
            original_consensus_transaction.payload.clone(),
        )
        .await
    }

    // Returns: (additional_utxos, total_Value_added)
    async fn more_utxos_for_merge_transaction(
        &self,
        utxo_manager: &MutexGuard<'_, UtxoManager>,
        original_consensus_transaction: &Transaction,
        original_selected_utxos: &[WalletUtxo],
        from_addresses: &[&WalletAddress],
        required_amount: u64,
        fee_rate: f64,
    ) -> WalletResult<(Vec<WalletUtxo>, u64)> {
        let dag_info = self.kaspa_client.get_block_dag_info().await.map_err(|e| {
            common::errors::RpcError::Transport {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            }
        })?;

        let mass_per_input = self
            .estimate_mass_per_input(&original_consensus_transaction.inputs[0])
            .await;
        let fee_per_input = (mass_per_input as f64 * fee_rate).ceil() as u64;

        let utxos_sorted_by_amount = utxo_manager.utxos_sorted_by_amount();
        let already_selected_utxos =
            HashSet::<WalletUtxo>::from_iter(original_selected_utxos.iter().cloned());

        let mut additional_utxos = vec![];
        let mut total_value_added = 0;
        for utxo in utxos_sorted_by_amount {
            if already_selected_utxos.contains(&utxo)
                || utxo_manager.is_utxo_unspendable(&utxo, dag_info.virtual_daa_score)
            {
                continue;
            }
            if !from_addresses.is_empty() && !from_addresses.contains(&&utxo.address) {
                continue;
            }

            additional_utxos.push(utxo.clone());
            total_value_added += utxo.utxo_entry.amount - fee_per_input;
            if total_value_added >= required_amount {
                break;
            }
        }

        if total_value_added < required_amount {
            Err(WalletError::from(TransactionError::InsufficientFunds {
                required_sompi: required_amount,
                available_sompi: total_value_added,
                location: ErrorLocation::capture(),
            }))
        } else {
            Ok((additional_utxos, total_value_added))
        }
    }

    // Returns: (split_count, input_per_split_count)
    async fn split_and_input_per_split_counts(
        &self,
        original_wallet_transaction: &WalletSignableTransaction,
        original_consensus_transaction: &SignableTransaction,
        transaction_mass: u64,
        change_address: &Address,
        fee_rate: f64,
        max_fee: u64,
    ) -> WalletResult<(usize, usize)> {
        // Create a dummy transaction which is a clone of the original transaction, but without inputs,
        // to calculate how much mass do all the inputs have
        let mut transaction_without_inputs = original_consensus_transaction.tx.clone();
        transaction_without_inputs.inputs = vec![];
        let mass_without_inputs = self.compute_mass_for_unsigned_consensus_transaction(
            &transaction_without_inputs,
            self.keys.minimum_signatures,
        );
        let mass_of_all_inputs = transaction_mass - mass_without_inputs;

        // Since the transaction was generated by kaspawallet, we assume all inputs have the same number of signatures, and
        // thus - the same mass.
        let input_count = original_consensus_transaction.tx.inputs.len() as u64;
        let mut mass_per_input = mass_of_all_inputs / input_count;
        if mass_of_all_inputs % input_count > 0 {
            mass_per_input += 1;
        }

        // Create another dummy transaction, this time one similar to the split transactions we wish to generate,
        // but with 0 inputs, to calculate how much mass for inputs do we have available in the split transactions
        let split_transaction_without_inputs = self
            .create_split_transaction(
                original_wallet_transaction,
                original_consensus_transaction,
                change_address,
                0,
                0,
                fee_rate,
                max_fee,
            )
            .await?;

        let mass_for_everything_except_inputs_in_split_transaction = self
            .compute_mass_for_unsigned_consensus_transaction(
                &split_transaction_without_inputs.transaction.inner().tx,
                self.keys.minimum_signatures,
            );

        let mass_for_inputs_in_split_transaction = MAXIMUM_STANDARD_TRANSACTION_MASS
            - mass_for_everything_except_inputs_in_split_transaction;

        let inputs_per_split_count = mass_for_inputs_in_split_transaction / mass_per_input;
        let mut split_count = input_count / inputs_per_split_count;
        if input_count % inputs_per_split_count > 0 {
            split_count += 1;
        }

        Ok((split_count as usize, inputs_per_split_count as usize))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_split_transaction(
        &self,
        original_wallet_transaction: &WalletSignableTransaction,
        original_consensus_transaction: &SignableTransaction,
        change_address: &Address,
        start_index: usize,
        end_index: usize,
        fee_rate: f64,
        max_fee: u64,
    ) -> WalletResult<WalletSignableTransaction> {
        let mut selected_utxos = vec![];
        let mut total_sompi = 0;

        for i in start_index..end_index {
            if i == original_consensus_transaction.tx.inputs.len() {
                break;
            }

            let input = &original_consensus_transaction.tx.inputs[i];
            let entry = original_consensus_transaction.entries[i].clone().unwrap();
            let utxo = WalletUtxo {
                outpoint: input.previous_outpoint.into(),
                utxo_entry: entry.into(),
                address: original_wallet_transaction.address_by_input_index[i].clone(),
            };
            total_sompi += utxo.utxo_entry.amount;
            selected_utxos.push(utxo);
        }
        if !selected_utxos.is_empty() {
            // selected utxos is empty when creating a dummy transaction for mass calculation
            let fee = self
                .estimate_fee(&selected_utxos, fee_rate, max_fee, total_sompi, &[])
                .await?;
            total_sompi -= fee;
        }

        let payment = WalletPayment {
            address: change_address.clone(),
            amount: total_sompi,
        };
        self.generate_unsigned_transaction(vec![payment], &selected_utxos, vec![])
            .await
    }

    fn check_transaction_fee_rate(
        &self,
        transaction: &WalletSignableTransaction,
        max_fee: u64,
    ) -> WalletResult<()> {
        let signable_transaction = transaction.transaction.inner();
        let total_ins: u64 = signable_transaction
            .entries
            .iter()
            .map(|entry| match entry {
                None => 0,
                Some(entry) => entry.amount,
            })
            .sum();

        let total_outs: u64 = signable_transaction
            .tx
            .outputs
            .iter()
            .map(|output| output.value)
            .sum();

        if total_ins < total_outs {
            return Err(WalletError::from(TransactionError::InsufficientFunds {
                required_sompi: total_outs,
                available_sompi: total_ins,
                location: ErrorLocation::capture(),
            }));
        };
        let fee = total_ins - total_outs;
        let mass =
            self.non_contextual_fee_mass(&signable_transaction.tx, self.keys.minimum_signatures);

        let fee_rate = fee as f64 / mass as f64;

        if fee_rate < 1.0 {
            Err(WalletError::from(TransactionError::FeeTooLow {
                provided_sompi: fee,
                required_sompi: mass,
                location: ErrorLocation::capture(),
            }))
        } else {
            let _ = max_fee;
            Ok(())
        }
    }

    pub(crate) async fn generate_unsigned_transaction(
        &self,
        payments: Vec<WalletPayment>,
        selected_utxos: &Vec<WalletUtxo>,
        payload: Vec<u8>,
    ) -> WalletResult<WalletSignableTransaction> {
        let mut sorted_extended_public_keys = self.keys.public_keys.clone();
        sorted_extended_public_keys.sort();

        let mut inputs = vec![];
        let mut utxo_entries = vec![];
        // DerivationPath lost Hash/Ord/Borsh derives upstream — track set
        // membership via the string representation (cheap, stable) so dedup
        // stays O(1) per insertion. The signer iterates the resulting Vec.
        let mut derivation_paths: Vec<DerivationPath> = vec![];
        let mut seen_paths: HashSet<String> = HashSet::new();
        let mut address_by_input_index = vec![];

        // Scope the address-manager lock to the input loop only — the
        // subsequent Transaction::new + finalize hashes inputs/outputs/
        // payload and can be expensive on large transactions; holding the
        // lock through that work blocks sync_manager and change_address.
        {
            let address_manager = self.address_manager.lock().await;
            for utxo in selected_utxos {
                let previous_outpoint =
                    TransactionOutpoint::new(utxo.outpoint.transaction_id, utxo.outpoint.index);
                // Build a v0 (sig_op_count) or v1 (compute_budget) input based on
                // the transaction version we will emit. v1 inputs that carry
                // sig_op_count are rejected by Toccata-era consensus.
                //
                // Both branches budget for `minimum_signatures` sig-ops per input.
                // That matches consensus exactly for 1-of-1 P2PK (the common
                // single-sig wallet shape). For an M-of-N multisig P2SH where
                // M < N, consensus counts N sig-ops in the redeem script while
                // we only commit M — pre-existing wallet behaviour the v1 path
                // intentionally mirrors so the migration adds zero new
                // regression surface. Fix is out of scope here.
                let input = if ComputeCommit::version_expects_compute_budget_field(self.tx_version)
                {
                    TransactionInput::new_with_compute_budget(
                        previous_outpoint,
                        vec![],
                        0,
                        self.compute_budget_per_input,
                    )
                } else {
                    TransactionInput::new(previous_outpoint, vec![], 0, self.minimum_signatures_u8)
                };
                inputs.push(input);

                let utxo_entry: UtxoEntry = utxo.utxo_entry.clone().into();
                utxo_entries.push(utxo_entry);
                let derivation_path = address_manager.calculate_address_path(&utxo.address)?;
                if seen_paths.insert(derivation_path.to_string()) {
                    derivation_paths.push(derivation_path);
                }
                address_by_input_index.push(utxo.address.clone());
            }
        }

        let mut outputs = vec![];
        let mut addresses_by_output_index = vec![];
        for payment in payments {
            let script_public_key = pay_to_address_script(&payment.address);
            let output = TransactionOutput::new(payment.amount, script_public_key);
            outputs.push(output);
            addresses_by_output_index.push(payment.address.clone());
        }

        let input_count = inputs.len();
        let output_count = outputs.len();
        let transaction = Transaction::new(
            self.tx_version,
            inputs,
            outputs,
            0,
            self.subnetwork_id,
            0,
            payload,
        );
        // Capture id before `transaction` is moved into `SignableTransaction`.
        // One info! per built tx — bounded, much lower volume than per-input.
        let tx_id = transaction.id();
        let mut signable_transaction = SignableTransaction::with_entries(transaction, utxo_entries);
        // Populate the (already proto-plumbed) non-contextual masses with the same signature-aware
        // values used to size the fee (shared `non_contextual_masses` helper), so the returned tx is
        // self-consistent for diagnostics / the rpc-provider. The node recomputes its own masses, so
        // this never affects acceptance.
        signable_transaction.calculated_non_contextual_masses = Some(
            self.non_contextual_masses(&signable_transaction.tx, self.keys.minimum_signatures),
        );
        let wallet_signable_transaction = WalletSignableTransaction::new_from_unsigned(
            signable_transaction,
            derivation_paths,
            address_by_input_index,
            addresses_by_output_index,
        );
        info!(
            tx_id = %tx_id,
            subnetwork_id = %self.subnetwork_id,
            tx_version = self.tx_version,
            compute_budget_per_input = self.compute_budget_per_input,
            input_count,
            output_count,
            "built unsigned tx"
        );

        Ok(wallet_signable_transaction)
    }

    // Returns: (fee_rate, max_fee)
    async fn default_fee_rate(&self) -> WalletResult<(f64, u64)> {
        let fee_estimate = self.kaspa_client.get_fee_estimate().await.map_err(|e| {
            common::errors::RpcError::Transport {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            }
        })?;
        Ok((fee_estimate.priority_bucket.feerate, SOMPI_PER_KASPA)) // Default to a bound of max 1 KAS as fee
    }

    async fn calculate_fee_limits(
        &self,
        fee_policy: Option<FeePolicy>,
    ) -> WalletResult<(f64, u64)> {
        // returns (fee_rate, max_fee)
        match fee_policy {
            Some(fee_policy) => match fee_policy.fee_policy {
                Some(fee_policy::FeePolicy::MaxFeeRate(requested_max_fee_rate)) => {
                    if requested_max_fee_rate < MIN_FEE_RATE {
                        return Err(WalletError::from(TransactionError::FeeTooLow {
                            provided_sompi: requested_max_fee_rate as u64,
                            required_sompi: MIN_FEE_RATE as u64,
                            location: ErrorLocation::capture(),
                        }));
                    }

                    let fee_estimate = self.kaspa_client.get_fee_estimate().await.map_err(|e| {
                        common::errors::RpcError::Transport {
                            reason: e.to_string(),
                            location: ErrorLocation::capture(),
                        }
                    })?;
                    let fee_rate =
                        f64::min(fee_estimate.priority_bucket.feerate, requested_max_fee_rate);
                    Ok((fee_rate, u64::MAX))
                }
                Some(fee_policy::FeePolicy::ExactFeeRate(requested_exact_fee_rate)) => {
                    if requested_exact_fee_rate < MIN_FEE_RATE {
                        return Err(WalletError::from(TransactionError::FeeTooLow {
                            provided_sompi: requested_exact_fee_rate as u64,
                            required_sompi: MIN_FEE_RATE as u64,
                            location: ErrorLocation::capture(),
                        }));
                    }

                    Ok((requested_exact_fee_rate, u64::MAX))
                }
                Some(fee_policy::FeePolicy::MaxFee(requested_max_fee)) => {
                    let fee_estimate = self.kaspa_client.get_fee_estimate().await.map_err(|e| {
                        common::errors::RpcError::Transport {
                            reason: e.to_string(),
                            location: ErrorLocation::capture(),
                        }
                    })?;
                    Ok((fee_estimate.priority_bucket.feerate, requested_max_fee))
                }
                None => self.default_fee_rate().await,
            },
            None => self.default_fee_rate().await,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn select_utxos(
        &mut self,
        utxo_manager: &MutexGuard<'_, UtxoManager>,
        preselected_utxos: &HashMap<WalletOutpoint, WalletUtxo>,
        amount: u64,
        is_send_all: bool,
        fee_rate: f64,
        max_fee: u64,
        from_addresses: &[&WalletAddress],
        payload: &[u8],
    ) -> WalletResult<(Vec<WalletUtxo>, u64, u64)> {
        debug!(
            "Selecting UTXOs for payment: from_address:{}, amount: {}, is_send_all: {}, fee_rate: {}, max_fee: {}",
            from_addresses.len(),
            amount,
            is_send_all,
            fee_rate,
            max_fee
        );
        let mut total_value = 0;
        let mut selected_utxos = vec![];

        let dag_info = self.kaspa_client.get_block_dag_info().await.map_err(|e| {
            common::errors::RpcError::Transport {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            }
        })?;

        let mut fee = 0;
        let mut fee_per_utxo = None;
        let mut iteration = async |transaction_generator: &mut TransactionGenerator,
                                   utxo_manager: &MutexGuard<UtxoManager>,
                                   utxo: &WalletUtxo|
               -> WalletResult<bool> {
            if !from_addresses.is_empty() && !from_addresses.contains(&&utxo.address) {
                return Ok(true);
            }
            if utxo_manager.is_utxo_unspendable(utxo, dag_info.virtual_daa_score) {
                return Ok(true);
            }

            selected_utxos.push(utxo.clone());
            total_value += utxo.utxo_entry.amount;
            let estimated_recipient_value = if is_send_all { total_value } else { amount };
            if fee_per_utxo.is_none() {
                fee_per_utxo = Some(
                    transaction_generator
                        .estimate_fee(
                            &selected_utxos,
                            fee_rate,
                            max_fee,
                            estimated_recipient_value,
                            payload,
                        )
                        .await?,
                );
            }
            fee += fee_per_utxo.unwrap();

            let total_spend = amount + fee;
            // Two break cases (if not send all):
            // 		1. total_value == totalSpend, so there's no change needed -> number of outputs = 1, so a single input is sufficient
            // 		2. total_value > totalSpend, so there will be change and 2 outputs, therefore in order to not struggle with --
            //		   2.1 go-nodes dust patch we try and find at least 2 inputs (even though the next one is not necessary in terms of spend value)
            // 		   2.2 KIP9 we try and make sure that the change amount is not too small
            if is_send_all {
                return Ok(true);
            }
            if total_value == total_spend {
                return Ok(false);
            }
            if total_value >= total_spend + MIN_CHANGE_TARGET && selected_utxos.len() > 1 {
                return Ok(false);
            }
            Ok(true)
        };
        let owned_utxos = utxo_manager.utxos_sorted_by_amount();
        let available_utxos: Vec<_> = if !preselected_utxos.is_empty() {
            preselected_utxos.values().collect()
        } else {
            owned_utxos.iter().collect()
        };
        for utxo in available_utxos {
            let should_continue = iteration(self, utxo_manager, utxo).await?;
            if !should_continue {
                break;
            }
        }

        let total_spend: u64;
        let total_received: u64;
        if is_send_all {
            total_spend = total_value;
            total_received = total_value - fee;
        } else {
            total_spend = amount + fee;
            total_received = amount;
        }

        if total_value < total_spend {
            return Err(WalletError::from(TransactionError::InsufficientFunds {
                required_sompi: total_spend,
                available_sompi: total_value,
                location: ErrorLocation::capture(),
            }));
        }
        if is_send_all && total_value == 0 {
            return Err(WalletError::from(TransactionError::InsufficientFunds {
                required_sompi: 0,
                available_sompi: 0,
                location: ErrorLocation::capture(),
            }));
        }

        debug!(
            "Selected {} UTXOS with total_received: {}, total_value: {}, total_spend: {}",
            selected_utxos.len(),
            total_received,
            total_value,
            total_spend
        );

        Ok((selected_utxos, total_received, total_value - total_spend))
    }

    async fn estimate_fee(
        &self,
        selected_utxos: &Vec<WalletUtxo>,
        fee_rate: f64,
        max_fee: u64,
        estimated_recipient_value: u64,
        payload: &[u8],
    ) -> WalletResult<u64> {
        let estimated_mass = self
            .estimate_mass(selected_utxos, estimated_recipient_value, payload)
            .await?;
        let calculated_fee = ((estimated_mass as f64) * (fee_rate)).ceil() as u64;
        let fee = min(calculated_fee, max_fee);
        Ok(fee)
    }

    /// Upstream's `calc_compute_mass_for_unsigned_consensus_transaction`
    /// still has an explicit `TODO: Add support for v1 transactions` and
    /// counts only `sig_op_count` for the per-input script-mass term — for
    /// v1 inputs this term is zero, undercounting fee by ~1000 grams per
    /// input. Apply a local compensation until upstream catches up.
    fn compute_mass_for_unsigned_consensus_transaction(
        &self,
        tx: &Transaction,
        minimum_signatures: u16,
    ) -> u64 {
        let base = self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(tx, minimum_signatures);
        if ComputeCommit::version_expects_compute_budget_field(tx.version) {
            let v1_script_mass: u64 = tx
                .inputs
                .iter()
                .map(|input| {
                    u64::from(input.compute_commit.compute_budget().unwrap_or(0))
                        * GRAMS_PER_COMPUTE_BUDGET_UNIT
                })
                .sum();
            base + v1_script_mass
        } else {
            base
        }
    }

    /// The transaction's non-contextual masses (compute + transient), computed with the same
    /// signature-aware accounting the node uses. Single source of truth shared by the fee-sizing path
    /// (`non_contextual_fee_mass`) and the diagnostic `calculated_non_contextual_masses` populated on
    /// the returned tx, so the two can never drift. Reuses the local compute helper (which carries the
    /// v1 `ComputeCommit` compensation) so the compute term is not double-counted, and pairs it with
    /// the byte-proportional transient term so poorly-compressible payload-heavy transactions are
    /// priced even when their compute mass is small.
    fn non_contextual_masses(
        &self,
        tx: &Transaction,
        minimum_signatures: u16,
    ) -> NonContextualMasses {
        let compute_mass =
            self.compute_mass_for_unsigned_consensus_transaction(tx, minimum_signatures);
        let transient_mass = estimate_transient_mass(tx, minimum_signatures);
        NonContextualMasses::new(compute_mass, transient_mass)
    }

    /// Node-equivalent relay-fee mass = `normalized_max(compute_mass, transient_mass)`, matching the
    /// Toccata node's standardness floor, which charges the relay fee on
    /// `max(compute, normalized_transient)` (`check_transaction_standard`).
    fn non_contextual_fee_mass(&self, tx: &Transaction, minimum_signatures: u16) -> u64 {
        self.non_contextual_masses(tx, minimum_signatures)
            .normalized_max(&self.mass_cofactors)
    }

    pub async fn estimate_mass(
        &self,
        selected_utxos: &Vec<WalletUtxo>,
        estimated_recipient_value: u64,
        payload: &[u8],
    ) -> WalletResult<u64> {
        let fake_public_key = &[0u8; 33];
        // We assume the worst case where the recipient address is ECDSA. In this case the scriptPubKey will be the longest.
        let fake_address = Address::new(self.address_prefix, Version::PubKeyECDSA, fake_public_key);

        let mut total_value = 0;
        for utxo in selected_utxos {
            total_value += utxo.utxo_entry.amount;
        }

        // This is an approximation for the distribution of value between the recipient output and the change output.
        let mock_payments = if total_value > estimated_recipient_value {
            vec![
                WalletPayment {
                    address: fake_address.clone(),
                    amount: estimated_recipient_value,
                },
                WalletPayment {
                    address: fake_address,
                    amount: total_value - estimated_recipient_value,
                },
            ]
        } else {
            vec![WalletPayment {
                address: fake_address,
                amount: total_value,
            }]
        };
        let mock_transaction = self
            .generate_unsigned_transaction(mock_payments, selected_utxos, payload.to_owned())
            .await?;

        let mass = self.non_contextual_fee_mass(
            &mock_transaction.transaction.inner().tx,
            self.keys.minimum_signatures,
        );
        Ok(mass)
    }

    pub async fn estimate_mass_per_input(&self, input: &TransactionInput) -> u64 {
        // Upstream's `calc_compute_mass_for_client_transaction_input` became
        // `pub(crate)` on the Toccata branch. Approximate the per-input
        // contribution locally: a wallet input serializes to roughly 64
        // bytes plus its sig-op cost. The result is used only for UTXO
        // selection / fee estimation — final fee uses
        // `calc_compute_mass_for_unsigned_consensus_transaction` end-to-end.
        //
        // v0 charges `sig_op_count * mass_per_sig_op`. v1 charges
        // `compute_budget * GRAMS_PER_COMPUTE_BUDGET_UNIT` — so the same
        // execution allowance produces the same mass.
        const APPROX_INPUT_BYTES: u64 = 64;
        const APPROX_MASS_PER_TX_BYTE: u64 = 1;
        let input_compute_mass = match (
            input.compute_commit.sig_op_count(),
            input.compute_commit.compute_budget(),
        ) {
            (Some(sig_ops), _) => sig_ops as u64 * self.mass_per_sig_op,
            (None, Some(cb)) => cb as u64 * GRAMS_PER_COMPUTE_BUDGET_UNIT,
            (None, None) => 0,
        };
        input_compute_mass
            + APPROX_INPUT_BYTES * APPROX_MASS_PER_TX_BYTE
            + self.signature_mass_per_input
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
    use std::str::FromStr;

    #[test]
    fn select_tx_version_native_subnetwork_uses_tx_version() {
        assert_eq!(select_tx_version(&SUBNETWORK_ID_NATIVE), TX_VERSION);
    }

    #[test]
    fn select_tx_version_non_native_subnetwork_uses_toccata() {
        let igra_lane = SubnetworkId::from_str("97b1000000000000000000000000000000000000").unwrap();
        assert_eq!(select_tx_version(&igra_lane), TX_VERSION_TOCCATA);
        assert_ne!(TX_VERSION, TX_VERSION_TOCCATA);
    }

    const TEST_MASS_PER_SIG_OP: u64 = 1000;

    #[test]
    fn compute_budget_for_single_signature_covers_one_sigop() {
        // mass_per_sig_op / GRAMS_PER_COMPUTE_BUDGET_UNIT = 1000 / 100 = 10
        // mirrors the upstream consensus-test pattern for a 1-sigop input.
        assert_eq!(
            compute_budget_for_signature(TEST_MASS_PER_SIG_OP, 1).unwrap(),
            10
        );
    }

    #[test]
    fn compute_budget_for_zero_signatures_is_zero() {
        // A 0-of-N keys configuration is invalid and rejected at
        // `TransactionGenerator::new`. The helper itself faithfully
        // returns `per_sig * 0 = 0`; the construction-time check is the
        // load-bearing gate.
        assert_eq!(
            compute_budget_for_signature(TEST_MASS_PER_SIG_OP, 0).unwrap(),
            0
        );
    }

    #[test]
    fn compute_budget_for_multisig_scales_with_minimum_signatures() {
        // Each additional required signature adds one sigop's worth of
        // budget (10 compute_budget units, equivalent to one v0 sig_op_count).
        assert_eq!(
            compute_budget_for_signature(TEST_MASS_PER_SIG_OP, 2).unwrap(),
            20
        );
        assert_eq!(
            compute_budget_for_signature(TEST_MASS_PER_SIG_OP, 3).unwrap(),
            30
        );
    }

    #[test]
    fn compute_budget_v0_v1_equivalence() {
        // v0 sig_op_count = N committed mass field maps to
        // `allowed_script_units = N * SCRIPT_UNITS_PER_SIGOP_COUNT_UNIT`,
        // i.e. `N * 100_000`. v1 commits `compute_budget = N*10` and maps to
        // `N*10 * SCRIPT_UNITS_PER_COMPUTE_BUDGET_UNIT = N*10 * 10_000 =
        // N * 100_000`. Same allowance, so single-sig wallets see no
        // behavioural change moving from v0 to v1.
        for n in [1u16, 2, 3, 5, 10] {
            let v0_units = n as u64 * 100_000;
            let v1_units =
                compute_budget_for_signature(TEST_MASS_PER_SIG_OP, n).unwrap() as u64 * 10_000;
            assert_eq!(v0_units, v1_units, "mismatch at minimum_signatures={n}");
        }
    }

    #[test]
    fn upstream_v1_mass_calc_still_needs_local_compensation() {
        // Canary: when upstream removes its `TODO: Add support for v1
        // transactions` and adds the script-mass term itself, this test
        // will fail and the maintainer must remove the local compensation
        // in `compute_mass_for_unsigned_consensus_transaction` to avoid
        // double-counting. Today the upstream helper returns the same
        // value for a single-input v0 tx (sig_op_count=0) as for the v1
        // counterpart (compute_budget=1) — proving the v1 term is still
        // missing from upstream's accounting.
        use kaspa_consensus_core::config::params::DEVNET_PARAMS;
        use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
        use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutpoint};
        let mc = MassCalculator::new(&DEVNET_PARAMS);
        let outpoint = TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([7u8; 32]), 0);
        let v0_tx = Transaction::new(
            0,
            vec![TransactionInput::new(outpoint, vec![], 0, 0)],
            vec![],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let v1_tx = Transaction::new(
            1,
            vec![TransactionInput::new_with_compute_budget(
                outpoint,
                vec![],
                0,
                1,
            )],
            vec![],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let v0 = mc.calc_compute_mass_for_unsigned_consensus_transaction(&v0_tx, 1);
        let v1 = mc.calc_compute_mass_for_unsigned_consensus_transaction(&v1_tx, 1);
        assert_eq!(
            v0, v1,
            "upstream calc_compute_mass_for_unsigned_consensus_transaction now \
             distinguishes v0 from v1 — remove the local compensation in \
             compute_mass_for_unsigned_consensus_transaction to avoid double-counting"
        );
    }

    #[test]
    fn estimate_transient_mass_includes_signature_bytes() {
        // The unsigned mock has empty signature scripts, but the node charges transient on the SIGNED
        // size — so the estimate must add `SIGNATURE_SIZE` per required signature per input.
        use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
        use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutpoint};
        let outpoint = TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([7u8; 32]), 0);
        let tx = Transaction::new(
            0,
            vec![TransactionInput::new(outpoint, vec![], 0, 0)],
            vec![],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let unsigned_size = transaction_estimated_serialized_size(&tx);
        assert_eq!(
            estimate_transient_mass(&tx, 1),
            (unsigned_size + SIGNATURE_SIZE) * TRANSIENT_BYTE_TO_MASS_FACTOR
        );
        // Each additional required signature adds another signature footprint per input.
        assert_eq!(
            estimate_transient_mass(&tx, 2),
            (unsigned_size + 2 * SIGNATURE_SIZE) * TRANSIENT_BYTE_TO_MASS_FACTOR
        );
    }

    #[test]
    fn estimate_transient_mass_scales_with_input_count() {
        // Transient mass tracks the SIGNED serialized size, which grows by one signature footprint
        // (`SIGNATURE_SIZE`) per required signature per input. A 3-input tx therefore carries 3× the
        // signature footprint of a single-input tx at the same `minimum_signatures` — guarding the
        // `* tx.inputs.len()` factor in `estimate_transient_mass`.
        use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
        use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutpoint};
        let input = |seed: u8| {
            let outpoint = TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([seed; 32]), 0);
            TransactionInput::new(outpoint, vec![], 0, 0)
        };
        let tx = Transaction::new(
            0,
            vec![input(1), input(2), input(3)],
            vec![],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let unsigned_size = transaction_estimated_serialized_size(&tx);
        // 2 required signatures × 3 inputs = 6 signature footprints added before scaling.
        assert_eq!(
            estimate_transient_mass(&tx, 2),
            (unsigned_size + 6 * SIGNATURE_SIZE) * TRANSIENT_BYTE_TO_MASS_FACTOR
        );
    }

    #[test]
    fn fee_mass_uses_normalized_transient_for_payload_heavy_tx() {
        // The exact failure case: a poorly-compressible payload-heavy tx. Transient (∝ byte size)
        // dominates compute, so the node-equivalent fee mass = normalized transient, which is strictly
        // greater than the old compute-only mass. Pre-fix the wallet returned `compute` and underpaid.
        use kaspa_consensus_core::config::params::DEVNET_PARAMS;
        use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
        use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutpoint};
        let mc = MassCalculator::new(&DEVNET_PARAMS);
        let cofactors = DEVNET_PARAMS.mempool_block_mass_cofactors().raw_post();
        let outpoint = TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([7u8; 32]), 0);
        let tx = Transaction::new(
            0,
            vec![TransactionInput::new(outpoint, vec![], 0, 0)],
            vec![],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![0xABu8; 6000],
        );
        let compute = mc.calc_compute_mass_for_unsigned_consensus_transaction(&tx, 1);
        let transient = estimate_transient_mass(&tx, 1);
        let fee_mass = NonContextualMasses::new(compute, transient).normalized_max(&cofactors);
        // DEVNET's transient cofactor = compute_block_limit / transient_block_limit = 0.5, so the node
        // scales transient down to `normalized_transient = ceil(transient * 0.5)` before taking the max
        // with compute. The load-bearing inequality the fix relies on is therefore
        // `normalized_transient >= compute` — assert that directly, not the raw `transient > compute`
        // (which does not by itself imply the normalized relationship).
        let normalized_transient =
            NonContextualMasses::new(0, transient).normalized_max(&cofactors);
        assert!(
            normalized_transient > compute,
            "normalized transient {normalized_transient} should dominate compute {compute} for a 6KB payload"
        );
        assert_eq!(
            fee_mass, normalized_transient,
            "fee mass must equal the normalized transient when transient dominates"
        );
        assert!(
            fee_mass > compute,
            "fee mass {fee_mass} must exceed the compute-only mass {compute} (the underpaying pre-fix value)"
        );
    }

    #[test]
    fn fee_mass_uses_compute_for_tiny_no_payload_tx() {
        // No-payload single-input tx: compute (sig-op + script mass) dominates, so the fee mass stays
        // the compute mass — no regression and no over-pay for ordinary small transactions.
        use kaspa_consensus_core::config::params::DEVNET_PARAMS;
        use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
        use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutpoint};
        let mc = MassCalculator::new(&DEVNET_PARAMS);
        let cofactors = DEVNET_PARAMS.mempool_block_mass_cofactors().raw_post();
        let outpoint = TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([7u8; 32]), 0);
        let tx = Transaction::new(
            0,
            vec![TransactionInput::new(outpoint, vec![], 0, 1)],
            vec![],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let compute = mc.calc_compute_mass_for_unsigned_consensus_transaction(&tx, 1);
        let transient = estimate_transient_mass(&tx, 1);
        let fee_mass = NonContextualMasses::new(compute, transient).normalized_max(&cofactors);
        let normalized_compute = NonContextualMasses::new(compute, 0).normalized_max(&cofactors);
        assert!(
            compute >= NonContextualMasses::new(0, transient).normalized_max(&cofactors),
            "compute should dominate for a tiny no-payload tx"
        );
        assert_eq!(
            fee_mass, normalized_compute,
            "fee mass must be the compute mass for tiny txs"
        );
    }

    #[test]
    fn compute_budget_for_overflow_returns_error() {
        // A pathological minimum_signatures that overflows u16 must be
        // rejected loudly rather than saturated silently.
        let err = compute_budget_for_signature(TEST_MASS_PER_SIG_OP, u16::MAX)
            .expect_err("overflow must be rejected");
        assert!(
            err.to_string().contains("overflow"),
            "expected overflow error, got: {err}"
        );
    }

    // Helper: Create a known test mnemonic
    //fn create_test_mnemonic() -> Mnemonic {
    //    let phrase = "decade minimum language dutch option narrow negative weird ball garbage purity guide weapon juice melt trash theory memory warrior rural okay flavor erosion senior";
    //    Mnemonic::new(phrase.to_string(), Language::English).unwrap()
    //}

    // TODO: Delete
    //#[rstest]
    //#[case(false)] // Schnorr
    //#[case(true)] // ECDSA
    //#[tokio::test]
    //async fn test_p2pk(#[case] ecdsa: bool) {
    //    // Create test consensus with no coinbase maturity
    //    let params = DEVNET_PARAMS; // TODO: Update test to check for all networks

    //    let mut consensus_config = ConsensusConfig::new(params);
    //    consensus_config.prior_coinbase_maturity = 0;
    //    consensus_config.crescendo.coinbase_maturity = 0;

    //    let tc = TestConsensus::new(&consensus_config);

    //    // Generate mnemonic and derive master key (not multisig)
    //    let mnemonic = create_test_mnemonic();
    //    let master_key = mnemonic_to_private_key(&mnemonic, false).unwrap();

    //    // Derive key for path "m/1/2/3"
    //    let derivation_path = DerivationPath::from_str("m/1/2/3").unwrap();
    //    let derived_key = master_key.derive_path(&derivation_path).unwrap();
    //    let public_key = derived_key.public_key();

    //    // Create P2PK address from public key
    //    let address_version = if ecdsa {
    //        Version::PubKeyECDSA
    //    } else {
    //        Version::PubKey
    //    };
    //    let address = Address::new(
    //        consensus_config.prefix(),
    //        address_version,
    //        &public_key.to_bytes(),
    //    );
    //    let script_public_key = pay_to_address_script(&address);

    //    // Add funding block with coinbase paying to our address
    //    let funding_block = tc
    //        .build_header_only_block_with_parents(0.into(), vec![params.genesis.hash])
    //        .to_immutable();
    //    let funding_block_status = tc
    //        .validate_and_insert_block(funding_block.clone())
    //        .virtual_state_task
    //        .await;
    //    assert_eq!(funding_block_status.unwrap(), BlockStatus::StatusUTXOValid);

    //    // Add maturity block
    //    let block1 = tc
    //        .build_header_only_block_with_parents(1.into(), vec![funding_block.header.hash])
    //        .to_immutable();
    //    let block1_status = tc
    //        .validate_and_insert_block(block1.clone())
    //        .virtual_state_task
    //        .await;
    //    assert_eq!(block1_status.unwrap(), BlockStatus::StatusUTXOValid);

    //    // Extract coinbase transaction and its output
    //    let coinbase_tx = &block1.transactions[0];
    //    let coinbase_output = &coinbase_tx.outputs[0];
    //    let coinbase_tx_id = coinbase_tx.id();

    //    // Create UTXO from the coinbase output
    //    let utxo = WalletUtxo {
    //        outpoint: WalletOutpoint {
    //            transaction_id: coinbase_tx_id.into(),
    //            index: 0,
    //        },
    //        utxo_entry: WalletUtxoEntry {
    //            amount: coinbase_output.value,
    //            script_public_key: coinbase_output.script_public_key.clone(),
    //            block_daa_score: funding_block.header.daa_score,
    //            is_coinbase: true,
    //        },
    //        address: WalletAddress::new(0, 0, Keychain::External),
    //    };

    //    // Create payment back to the same address (10 sompi)
    //    let payment = WalletPayment {
    //        address,
    //        amount: 10,
    //    };

    //    // Generate unsigned transaction
    //    let unsigned_tx_result = generate_unsigned_transaction(
    //        vec![payment],
    //        vec![utxo],
    //        script_public_key.clone().into(),
    //        1,    // priority_fee_sompi
    //        None, // payload
    //    )
    //    .await;

    //    assert!(
    //        unsigned_tx_result.is_ok(),
    //        "Failed to generate unsigned transaction: {:?}",
    //        unsigned_tx_result.err()
    //    );

    //    let unsigned_tx = unsigned_tx_result.unwrap();

    //    // Sign the transaction
    //    let private_key_bytes = derived_key.private_key().secret_bytes();
    //    let signed_tx = sign_with_multiple(unsigned_tx.transaction.into_inner(), &[private_key_bytes]);

    //    // Verify transaction is fully signed
    //    assert!(
    //        matches!(signed_tx, Signed::Fully(_)),
    //        "Transaction should be fully signed"
    //    );

    //    // Extract the signed transaction
    //    let signed_tx_inner = signed_tx.unwrap();

    //    // Add block with signed transaction
    //    let signed_block_hash = tc
    //        .add_block_with_parents(vec![maturity_block_hash], vec![signed_tx_inner.clone()])
    //        .unwrap();

    //    // Verify transaction was accepted in the DAG
    //    let virtual_state = tc.get_virtual_state_from_genesis().await.unwrap();
    //    let signed_tx_id = signed_tx_inner.id();

    //    // Check if the transaction's output was added to virtual UTXO set
    //    let expected_utxo = TransactionOutpoint::new(signed_tx_id, 0);
    //    let utxo_exists = tc.get_virtual_utxos(vec![expected_utxo]).await.is_ok();

    //    assert!(utxo_exists, "Transaction wasn't accepted in the DAG");

    //    tc.shutdown().await;
    //}
}
