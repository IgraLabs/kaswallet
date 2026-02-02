use crate::address_manager::AddressManager;
use crate::utxo_manager::UtxoManager;
use common::errors::WalletError::{SanityCheckFailed, UserInputError};
use common::errors::{ResultExt, WalletError, WalletResult};
use common::keys::Keys;
use common::model::{
    WalletAddress, WalletOutpoint, WalletPayment, WalletSignableTransaction, WalletUtxo,
    WalletUtxoEntry,
};
use itertools::Itertools;
use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::constants::{SOMPI_PER_KASPA, UNACCEPTED_DAA_SCORE};
use kaspa_consensus_core::tx::{
    SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput,
    UtxoEntry,
};
use kaspa_grpc_client::GrpcClient;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_txscript::pay_to_address_script;
use kaspa_wallet_core::prelude::AddressPrefix;
use kaspa_wallet_core::tx::{MAXIMUM_STANDARD_TRANSACTION_MASS, MassCalculator};
use log::debug;
use proto::kaswallet_proto::{FeePolicy, Outpoint, TransactionDescription, fee_policy};
use std::cmp::min;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};

// The current minimal fee rate according to mempool standards
const MIN_FEE_RATE: f64 = 1.0;

// The minimal change amount to target in order to avoid large storage mass (see KIP9 for more details).
// By having at least 10KAS in the change output we make sure that the storage mass charged for change is
// at most 1000 gram. Generally, if the payment is above 10KAS as well, the resulting storage mass will be
// in the order of magnitude of compute mass and wil not incur additional charges.
// Additionally, every transaction with send value > ~0.1 KAS should succeed (at most ~99K storage mass for payment
// output, thus overall lower than standard mass upper bound which is 100K gram)
const MIN_CHANGE_TARGET: u64 = SOMPI_PER_KASPA * 10;

pub struct TransactionGenerator {
    kaspa_client: Arc<GrpcClient>,
    keys: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    mass_calculator: Arc<MassCalculator>,
    address_prefix: AddressPrefix,

    signature_mass_per_input: u64,
}

impl TransactionGenerator {
    pub fn new(
        kaspa_client: Arc<GrpcClient>,
        keys: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        mass_calculator: Arc<MassCalculator>,
        address_prefix: AddressPrefix,
    ) -> Self {
        let signature_mass_per_input =
            mass_calculator.calc_compute_mass_for_signature(keys.minimum_signatures);
        Self {
            kaspa_client,
            keys,
            address_manager,
            mass_calculator,
            address_prefix,
            signature_mass_per_input,
        }
    }

    pub async fn create_unsigned_transactions(
        &mut self,
        utxo_manager: &MutexGuard<'_, UtxoManager>,
        transaction_description: TransactionDescription,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        let validate_address = |address_string, name| -> WalletResult<Address> {
            match Address::try_from(address_string) {
                Ok(address) => Ok(address),
                Err(e) => Err(UserInputError(format!("Invalid {} address: {}", name, e))),
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
            return Err(UserInputError(
                "Cannot specify both from_addresses and utxos".to_string(),
            ));
        }

        let from_addresses = if transaction_description.from_addresses.is_empty() {
            vec![]
        } else {
            let mut from_addresses = vec![];
            for address_string in transaction_description.from_addresses {
                let wallet_address = address_set.get(&address_string).ok_or_else(|| {
                    UserInputError(format!(
                        "From address is not in address set: {}",
                        address_string
                    ))
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
                if let Some(utxo) = utxos_by_outpoint.get(&preselected_outpoint.clone().into()) {
                    preselected_utxos.insert(utxo.outpoint.clone(), utxo.clone());
                } else {
                    return Err(UserInputError(format!(
                        "Preselected UTXO {:?} is not in UTXO set",
                        preselected_outpoint
                    )));
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

        debug!(
            "Selected utxos: {}",
            selected_utxos.iter().map(|utxo| &utxo.outpoint).join(", ")
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

        let original_consensus_transaction = original_wallet_transaction.transaction.unwrap_ref();

        let transaction_mass = self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
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
            return Err(WalletError::SanityCheckFailed(format!(
                "Original transaction has {} outputs, while 1 or 2 are expected",
                num_outputs
            )));
        }

        let mut total_value = 0u64;
        let mut utxos_from_split_transactions = vec![];

        for split_transaction in split_transactions {
            let split_consensus_transaction = split_transaction.transaction.unwrap_ref();
            let split_consensus_transaction = &split_consensus_transaction.tx;
            let output = &split_consensus_transaction.outputs[0];
            let utxo = WalletUtxo {
                outpoint: WalletOutpoint {
                    transaction_id: split_transaction.transaction.unwrap_ref().id(),
                    index: 0,
                },
                utxo_entry: WalletUtxoEntry {
                    amount: output.value,
                    script_public_key: output.script_public_key.clone(),
                    block_daa_score: UNACCEPTED_DAA_SCORE,
                    is_coinbase: false,
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
                    "Reducing sent value by {} to accomodate for merge transaction fee",
                    required_amount
                );
                available_value -= required_amount;
                sent_value -= required_amount;
                vec![]
            } else if !preselected_utxo_outpoints.is_empty() {
                return Err(UserInputError(
                    "Insufficient funds in pre-selected utxos for merge transaction fees"
                        .to_string(),
                ));
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
        let dag_info = self
            .kaspa_client
            .get_block_dag_info()
            .await
            .to_wallet_result_internal()?;

        let mass_per_input = self
            .estimate_mass_per_input(&original_consensus_transaction.inputs[0])
            .await;
        let fee_per_input = (mass_per_input as f64 * fee_rate).ceil() as u64;

        let already_selected_outpoints = HashSet::<WalletOutpoint>::from_iter(
            original_selected_utxos
                .iter()
                .map(|utxo| utxo.outpoint.clone()),
        );

        let mut additional_utxos = vec![];
        let mut total_value_added = 0;
        for utxo in utxo_manager.utxos_sorted_by_amount() {
            if already_selected_outpoints.contains(&utxo.outpoint)
                || utxo_manager.is_utxo_pending(utxo, dag_info.virtual_daa_score)
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
            Err(UserInputError(
                "Insufficient funds for merge transaction fees".to_string(),
            ))
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
        let mut trasnaction_without_inputs = original_consensus_transaction.tx.clone();
        trasnaction_without_inputs.inputs = vec![];
        let mass_without_inputs = self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
                &trasnaction_without_inputs,
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
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
                &split_transaction_without_inputs.transaction.unwrap_ref().tx,
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
        let signable_transaction = transaction.transaction.unwrap_ref();
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
            return Err(SanityCheckFailed(
                "transaction doesn't have enough funds to pay for the outputs".to_string(),
            ));
        };
        let fee = total_ins - total_outs;
        let mass = self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
                &signable_transaction.tx,
                self.keys.minimum_signatures,
            );

        let fee_rate = fee as f64 / mass as f64;

        if fee_rate < 1.0 {
            Err(UserInputError(format!(
                "setting max-fee to {} results in a fee rate of {}, which is below the minimum allowed fee rate of 1 sompi/gram",
                max_fee, fee_rate
            )))
        } else {
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
        let mut derivation_paths = HashSet::new();
        let mut address_by_input_index = vec![];

        for utxo in selected_utxos {
            let previous_outpoint =
                TransactionOutpoint::new(utxo.outpoint.transaction_id, utxo.outpoint.index);
            let input = TransactionInput::new(
                previous_outpoint,
                vec![],
                0,
                self.keys.minimum_signatures as u8,
            );
            inputs.push(input);

            let utxo_entry: UtxoEntry = utxo.utxo_entry.clone().into();
            utxo_entries.push(utxo_entry);
            {
                let address_manager = self.address_manager.lock().await;
                let derivation_path = address_manager.calculate_address_path(&utxo.address)?;
                derivation_paths.insert(derivation_path);
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

        let transaction = Transaction::new(0, inputs, outputs, 0, Default::default(), 0, payload);
        let signable_transaction = SignableTransaction::with_entries(transaction, utxo_entries);
        let wallet_signable_transaction = WalletSignableTransaction::new_from_unsigned(
            signable_transaction,
            derivation_paths,
            address_by_input_index,
            addresses_by_output_index,
        );

        Ok(wallet_signable_transaction)
    }

    // Returns: (fee_rate, max_fee)
    async fn default_fee_rate(&self) -> WalletResult<(f64, u64)> {
        let fee_estimate = self
            .kaspa_client
            .get_fee_estimate()
            .await
            .to_wallet_result_internal()?;
        Ok((fee_estimate.normal_buckets[0].feerate, SOMPI_PER_KASPA)) // Default to a bound of max 1 KAS as fee
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
                        return Err(UserInputError(format!(
                            "requested max fee rate {} is too low, minimum fee rate is {}",
                            requested_max_fee_rate, MIN_FEE_RATE
                        )));
                    }

                    let fee_estimate = self
                        .kaspa_client
                        .get_fee_estimate()
                        .await
                        .to_wallet_result_internal()?;
                    let fee_rate = f64::min(
                        fee_estimate.normal_buckets[0].feerate,
                        requested_max_fee_rate,
                    );
                    Ok((fee_rate, u64::MAX))
                }
                Some(fee_policy::FeePolicy::ExactFeeRate(requested_exact_fee_rate)) => {
                    if requested_exact_fee_rate < MIN_FEE_RATE {
                        return Err(UserInputError(format!(
                            "requested max fee rate {} is too low, minimum fee rate is {}",
                            requested_exact_fee_rate, MIN_FEE_RATE
                        )));
                    }

                    Ok((requested_exact_fee_rate, u64::MAX))
                }
                Some(fee_policy::FeePolicy::MaxFee(requested_max_fee)) => {
                    let fee_estimate = self
                        .kaspa_client
                        .get_fee_estimate()
                        .await
                        .to_wallet_result_internal()?;
                    Ok((fee_estimate.normal_buckets[0].feerate, requested_max_fee))
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

        let dag_info = self
            .kaspa_client
            .get_block_dag_info()
            .await
            .to_wallet_result_internal()?;

        let mut fee = 0;
        let mut fee_per_utxo = None;
        let mut iteration = async |transaction_generator: &mut TransactionGenerator,
                                   utxo_manager: &MutexGuard<UtxoManager>,
                                   utxo: &WalletUtxo|
               -> WalletResult<bool> {
            if !from_addresses.is_empty() && !from_addresses.contains(&&utxo.address) {
                return Ok(true);
            }
            if utxo_manager.is_utxo_pending(utxo, dag_info.virtual_daa_score) {
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
        if !preselected_utxos.is_empty() {
            for utxo in preselected_utxos.values() {
                let should_continue = iteration(self, utxo_manager, utxo).await?;
                if !should_continue {
                    break;
                }
            }
        } else {
            for utxo in utxo_manager.utxos_sorted_by_amount() {
                let should_continue = iteration(self, utxo_manager, utxo).await?;
                if !should_continue {
                    break;
                }
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
            return Err(UserInputError(format!(
                "Insufficient funds for send: {} required, while only {} available",
                amount / SOMPI_PER_KASPA,
                total_value / SOMPI_PER_KASPA
            )));
        }
        if is_send_all && total_value == 0 {
            return Err(UserInputError("No funds to send".to_string()));
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

        let mass = self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
                &mock_transaction.transaction.unwrap_ref().tx,
                self.keys.minimum_signatures,
            );
        Ok(mass)
    }

    pub async fn estimate_mass_per_input(&self, input: &TransactionInput) -> u64 {
        self.mass_calculator
            .calc_compute_mass_for_client_transaction_input(input)
            + self.signature_mass_per_input
    }
}

#[cfg(test)]
mod tests {

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
    //    let signed_tx = sign_with_multiple(unsigned_tx.transaction.unwrap(), &[private_key_bytes]);

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
