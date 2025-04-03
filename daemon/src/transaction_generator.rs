use crate::address_manager::AddressManager;
use crate::model::{
    WalletAddress, WalletOutpoint, WalletPayment, WalletSignableTransaction, WalletUtxo,
    WalletUtxoEntry,
};
use crate::utxo_manager::UtxoManager;
use chrono::{DateTime, Duration, Utc};
use common::errors::WalletError;
use common::keys::Keys;
use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::constants::{SOMPI_PER_KASPA, UNACCEPTED_DAA_SCORE};
use kaspa_consensus_core::tx::{
    SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput,
    UtxoEntry,
};
use kaspa_txscript::pay_to_address_script;
use kaspa_wallet_core::prelude::AddressPrefix;
use kaspa_wallet_core::tx::{MassCalculator, MAXIMUM_STANDARD_TRANSACTION_MASS};
use kaspa_wrpc_client::prelude::RpcApi;
use kaspa_wrpc_client::KaspaRpcClient;
use kaswallet_proto::kaswallet_proto::{fee_policy, FeePolicy, Outpoint};
use log::debug;
use std::cmp::min;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ops::Add;
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
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    keys: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    utxo_manager: Arc<Mutex<UtxoManager>>,
    mass_calculator: Arc<MassCalculator>,
    address_prefix: AddressPrefix,

    signature_mass_per_input: u64,

    used_outpoints: HashMap<WalletOutpoint, DateTime<Utc>>,
}

impl TransactionGenerator {
    pub fn new(
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        keys: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<Mutex<UtxoManager>>,
        mass_calculator: Arc<MassCalculator>,
        address_prefix: AddressPrefix,
    ) -> Self {
        let signature_mass_per_input =
            mass_calculator.calc_compute_mass_for_signature(keys.minimum_signatures);
        Self {
            kaspa_rpc_client,
            keys,
            address_manager,
            utxo_manager,
            mass_calculator,
            address_prefix,
            signature_mass_per_input,
            used_outpoints: HashMap::new(),
        }
    }

    pub async fn create_unsigned_transactions(
        &mut self,
        to_address: String,
        amount: u64,
        is_send_all: bool,
        payload: Vec<u8>,
        from_addresses_strings: Vec<String>,
        preselected_utxo_outpoints: Vec<Outpoint>,
        use_existing_change_address: bool,
        fee_policy: Option<FeePolicy>,
    ) -> Result<Vec<WalletSignableTransaction>, Box<dyn Error + Send + Sync>> {
        let validate_address =
            |address_string, name| -> Result<Address, Box<dyn Error + Send + Sync>> {
                match Address::try_from(address_string) {
                    Ok(address) => Ok(address),
                    Err(e) => Err(Box::new(WalletError::UserInputError(format!(
                        "Invalid {} address: {}",
                        name, e
                    )))),
                }
            };

        let to_address = validate_address(to_address, "to")?;
        let address_set: HashMap<String, WalletAddress>;
        {
            let address_manager = self.address_manager.lock().await;
            address_set = address_manager.address_set().await;
        }

        if !from_addresses_strings.is_empty() && !preselected_utxo_outpoints.is_empty() {
            return Err(Box::new(WalletError::UserInputError(
                "Cannot specify both from_addresses and utxos".to_string(),
            )));
        }

        let from_addresses = if from_addresses_strings.is_empty() {
            vec![]
        } else {
            let mut from_addresses = vec![];
            for address_string in from_addresses_strings {
                let wallet_address = address_set.get(&address_string).ok_or_else(|| {
                    WalletError::UserInputError(format!(
                        "From address is not in address set: {}",
                        address_string
                    ))
                })?;
                from_addresses.push(wallet_address);
            }
            from_addresses
        };
        let preselected_utxos = if preselected_utxo_outpoints.is_empty() {
            HashMap::new()
        } else {
            let mut preselected_utxos = HashMap::new();
            {
                let utxo_manager = self.utxo_manager.lock().await;
                let utxos_by_outpoint = utxo_manager.utxos_by_outpoint();
                for outpoint in &preselected_utxo_outpoints {
                    if let Some(utxo) = utxos_by_outpoint.get(&outpoint.clone().into()) {
                        let utxo = utxo.clone();
                        preselected_utxos.insert(utxo.outpoint.clone(), utxo);
                    } else {
                        return Err(Box::new(WalletError::UserInputError(format!(
                            "UTXO {:?} is not in UTXO set",
                            outpoint
                        ))));
                    }
                }
                preselected_utxos
            }
        };

        let (fee_rate, max_fee) = self.calculate_fee_limits(fee_policy).await?;

        let change_address: Address;
        let change_wallet_address: WalletAddress;
        {
            let address_manager = self.address_manager.lock().await;
            (change_address, change_wallet_address) = // TODO: check if I really need both.
                address_manager.change_address(use_existing_change_address, &from_addresses).await?;
        }

        let selected_utxos: Vec<WalletUtxo>;
        let amount_sent_to_recipient: u64;
        let change_sompi: u64;
        (selected_utxos, amount_sent_to_recipient, change_sompi) = self
            .select_utxos(
                &preselected_utxos,
                HashSet::new(),
                amount,
                is_send_all,
                fee_rate,
                max_fee,
                &from_addresses,
                &payload,
            )
            .await?;

        let mut payments = vec![WalletPayment::new(
            to_address.clone(),
            amount_sent_to_recipient,
        )];
        if change_sompi > 0 {
            payments.push(WalletPayment::new(change_address.clone(), change_sompi));
        }
        let unsigned_transaction = self
            .generate_unsigned_transaction(payments, &selected_utxos, payload)
            .await?;

        let unsigned_transactions = self
            .maybe_auto_compound_transaction(
                unsigned_transaction,
                &selected_utxos,
                from_addresses,
                &to_address,
                is_send_all,
                &preselected_utxo_outpoints,
                &change_address,
                &change_wallet_address,
                fee_rate,
                max_fee,
            )
            .await?;

        Ok(unsigned_transactions)
    }

    async fn maybe_auto_compound_transaction(
        &self,
        original_wallet_transaction: WalletSignableTransaction,
        original_selected_utxos: &Vec<WalletUtxo>,
        from_addresses: Vec<&WalletAddress>,
        to_address: &Address,
        is_send_all: bool,
        preselected_utxo_outpoints: &Vec<Outpoint>,
        change_address: &Address,
        change_wallet_address: &WalletAddress,
        fee_rate: f64,
        max_fee: u64,
    ) -> Result<Vec<WalletSignableTransaction>, Box<dyn Error + Send + Sync>> {
        self.check_transaction_fee_rate(&original_wallet_transaction, max_fee)?;

        let orignal_consensus_transaction = original_wallet_transaction.transaction.unwrap_ref();

        let transaction_mass = self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
                &orignal_consensus_transaction.tx,
                self.keys.minimum_signatures,
            );

        if transaction_mass < MAXIMUM_STANDARD_TRANSACTION_MASS {
            debug!("No need to auto-compound transaction");
            return Ok(vec![original_wallet_transaction]);
        }

        let (split_count, input_per_split_count) = self
            .split_and_input_per_split_counts(
                &original_wallet_transaction,
                &orignal_consensus_transaction,
                transaction_mass,
                &change_address,
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
                    orignal_consensus_transaction,
                    &change_address,
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
                &split_transactions,
                &orignal_consensus_transaction.tx,
                original_selected_utxos,
                &from_addresses,
                to_address,
                is_send_all,
                preselected_utxo_outpoints,
                change_address,
                change_wallet_address,
                fee_rate,
                max_fee,
            )
            .await?;

        // Recursion will be 2-3 iterations deep even in the rarest` cases, so considered safe..
        let split_merge_transaction = Box::pin(self.maybe_auto_compound_transaction(
            merge_transaction,
            original_selected_utxos,
            from_addresses,
            to_address,
            is_send_all,
            preselected_utxo_outpoints,
            change_address,
            change_wallet_address,
            fee_rate,
            max_fee,
        ))
        .await?;

        let split_transactions = [split_transactions, split_merge_transaction]
            .concat()
            .to_vec();

        Ok(split_transactions)
    }
    async fn merge_transaction(
        &self,
        split_transactions: &Vec<WalletSignableTransaction>,
        original_consensus_transaction: &Transaction,
        original_selected_utxos: &Vec<WalletUtxo>,
        from_addresses: &Vec<&WalletAddress>,
        to_address: &Address,
        is_send_all: bool,
        preselected_utxo_outpoints: &Vec<Outpoint>,
        change_address: &Address,
        change_wallet_address: &WalletAddress,
        fee_rate: f64,
        max_fee: u64,
    ) -> Result<WalletSignableTransaction, Box<dyn Error + Send + Sync>> {
        let num_outputs = original_consensus_transaction.outputs.len();
        if ![1, 2].contains(&num_outputs) {
            // This is a sanity check to make sure originalTransaction has either 1 or 2 outputs:
            // 1. For the payment itself
            // 2. (optional) for change
            return Err(Box::new(WalletError::SanityCheckFailed(format!(
                "Original transactin has {} outputs, while 1 or 2 are expected",
                num_outputs
            ))));
        }

        let mut total_value = 0u64;
        let mut sent_value = original_consensus_transaction.outputs[0].value;
        let mut utxos_from_split_transactions = vec![];

        for split_transaction in split_transactions {
            let split_consensus_transaction = (&split_transaction.transaction).unwrap_ref();
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
                sent_value,
                &original_consensus_transaction.payload,
            )
            .await?;

        total_value -= merge_transaction_fee;

        if total_value < sent_value {
            let required_amount = sent_value - total_value;
            if is_send_all {
                debug!(
                    "Reducing sent value by {} to accomodate for merge transaction fee",
                    required_amount
                );
                sent_value -= required_amount;
            } else if !preselected_utxo_outpoints.is_empty() {
                return Err(Box::new(WalletError::UserInputError(
                    "Insufficient funds in pre-selected utxos for merge transaction fees"
                        .to_string(),
                )));
            } else {
                debug!(
                    "Adding more UTXOs to the merge transaction to cover fee; required amount: {}",
                    required_amount
                );
                // Sometimes the fees from compound transactions make the total output higher than what's
                // available from selected utxos, in such cases - find one more UTXO and use it.
                let (additional_utxos, total_value_added) = self
                    .more_utxos_for_merge_transaction(
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
                utxos_from_split_transactions =
                    [utxos_from_split_transactions, additional_utxos].concat();
                total_value += total_value_added;
            }
        }

        let mut payments = vec![WalletPayment {
            address: to_address.clone(),
            amount: sent_value,
        }];

        if total_value > sent_value {
            payments.push(WalletPayment {
                address: change_address.clone(),
                amount: total_value - sent_value,
            });
        }

        self.generate_unsigned_transaction(
            payments,
            &utxos_from_split_transactions,
            original_consensus_transaction.payload.clone(),
        )
        .await
    }

    // Returns: (additional_utxos, total_Value_added)
    async fn more_utxos_for_merge_transaction(
        &self,
        original_consensus_transaction: &Transaction,
        original_selected_utxos: &Vec<WalletUtxo>,
        from_addresses: &Vec<&WalletAddress>,
        required_amount: u64,
        fee_rate: f64,
    ) -> Result<(Vec<WalletUtxo>, u64), Box<dyn Error + Send + Sync>> {
        let dag_info = self.kaspa_rpc_client.get_block_dag_info().await?;

        let mass_per_input = self
            .estimate_mass_per_input(&original_consensus_transaction.inputs[0])
            .await;
        let fee_per_input = (mass_per_input as f64 * fee_rate).ceil() as u64;

        let utxo_manager = self.utxo_manager.lock().await;
        let utxos_sorted_by_amount = utxo_manager.utxos_sorted_by_amount();
        let already_selected_utxos =
            HashSet::<WalletUtxo>::from_iter(original_selected_utxos.iter().cloned());

        let mut additional_utxos = vec![];
        let mut total_value_added = 0;
        for utxo in utxos_sorted_by_amount {
            if already_selected_utxos.contains(utxo)
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
            Err(Box::new(WalletError::UserInputError(
                "Insufficient funds for merge transaction fees".to_string(),
            )))
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
    ) -> Result<(usize, usize), Box<dyn Error + Send + Sync>> {
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

    async fn create_split_transaction(
        &self,
        original_wallet_transaction: &WalletSignableTransaction,
        original_consensus_transaction: &SignableTransaction,
        change_address: &Address,
        start_index: usize,
        end_index: usize,
        fee_rate: f64,
        max_fee: u64,
    ) -> Result<WalletSignableTransaction, Box<dyn Error + Send + Sync>> {
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

        if selected_utxos.len() > 0 {
            let fee = self
                .estimate_fee(&selected_utxos, fee_rate, max_fee, total_sompi, &vec![])
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
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
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
            return Err(Box::new(WalletError::SanityCheckFailed(
                "transaction doesn't have enough funds to pay for the outputs".to_string(),
            )));
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
            Err(Box::new(WalletError::UserInputError(format!(
                "setting max-fee to {} results in a fee rate of {}, which is below the minimum allowed fee rate of 1 sompi/gram",
                max_fee, fee_rate
            ))))
        } else {
            Ok(())
        }
    }

    async fn generate_unsigned_transaction(
        &self,
        payments: Vec<WalletPayment>,
        selected_utxos: &Vec<WalletUtxo>,
        payload: Vec<u8>,
    ) -> Result<WalletSignableTransaction, Box<dyn Error + Send + Sync>> {
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
        for payment in payments {
            let script_public_key = pay_to_address_script(&payment.address);
            let output = TransactionOutput::new(payment.amount, script_public_key);
            outputs.push(output);
        }

        let transaction = Transaction::new(0, inputs, outputs, 0, Default::default(), 0, payload);
        let signable_transaction = SignableTransaction::with_entries(transaction, utxo_entries);
        let wallet_signable_transaction = WalletSignableTransaction::new_from_unsigned(
            signable_transaction.clone(),
            derivation_paths,
            address_by_input_index,
        );

        Ok(wallet_signable_transaction)
    }

    // Returns: (fee_rate, max_fee)
    async fn default_fee_rate(&self) -> Result<(f64, u64), Box<dyn Error + Send + Sync>> {
        let fee_estimate = self.kaspa_rpc_client.get_fee_estimate().await?;
        Ok((fee_estimate.normal_buckets[0].feerate, SOMPI_PER_KASPA)) // Default to a bound of max 1 KAS as fee
    }

    async fn calculate_fee_limits(
        &self,
        fee_policy: Option<FeePolicy>,
    ) -> Result<(f64, u64), Box<dyn Error + Send + Sync>> {
        // returns (fee_rate, max_fee)
        match fee_policy {
            Some(fee_policy) => match fee_policy.fee_policy {
                Some(fee_policy::FeePolicy::MaxFeeRate(requested_max_fee_rate)) => {
                    if requested_max_fee_rate < MIN_FEE_RATE {
                        return Err(Box::new(WalletError::UserInputError(format!(
                            "requested max fee rate {} is too low, minimum fee rate is {}",
                            requested_max_fee_rate, MIN_FEE_RATE
                        ))));
                    }

                    let fee_estimate = self.kaspa_rpc_client.get_fee_estimate().await?;
                    let fee_rate = f64::min(
                        fee_estimate.normal_buckets[0].feerate,
                        requested_max_fee_rate,
                    );
                    Ok((fee_rate, u64::MAX))
                }
                Some(fee_policy::FeePolicy::ExactFeeRate(requested_exact_fee_rate)) => {
                    if requested_exact_fee_rate < MIN_FEE_RATE {
                        return Err(Box::new(WalletError::UserInputError(format!(
                            "requested max fee rate {} is too low, minimum fee rate is {}",
                            requested_exact_fee_rate, MIN_FEE_RATE
                        ))));
                    }

                    Ok((requested_exact_fee_rate, u64::MAX))
                }
                Some(fee_policy::FeePolicy::MaxFee(requested_max_fee)) => {
                    let fee_estimate = self.kaspa_rpc_client.get_fee_estimate().await?;
                    Ok((fee_estimate.normal_buckets[0].feerate, requested_max_fee))
                }
                None => self.default_fee_rate().await,
            },
            None => self.default_fee_rate().await,
        }
    }

    pub async fn select_utxos(
        &mut self,
        preselected_utxos: &HashMap<WalletOutpoint, WalletUtxo>,
        allowed_used_outpoints: HashSet<WalletOutpoint>,
        amount: u64,
        is_send_all: bool,
        fee_rate: f64,
        max_fee: u64,
        from_addresses: &Vec<&WalletAddress>,
        payload: &Vec<u8>,
    ) -> Result<(Vec<WalletUtxo>, u64, u64), Box<dyn Error + Send + Sync>> {
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

        let dag_info = self.kaspa_rpc_client.get_block_dag_info().await?;

        let mut fee = 0;
        let start_time_of_last_completed_refresh: DateTime<Utc>;
        {
            let utxo_manager = self.utxo_manager.lock().await;
            start_time_of_last_completed_refresh =
                utxo_manager.start_time_of_last_completed_refresh();
        }
        let mut iteration = async |transaction_generator: &mut TransactionGenerator,
                                   utxo_manager: &MutexGuard<UtxoManager>,
                                   utxo: &WalletUtxo,
                                   avoid_preselected: bool|
               -> Result<bool, Box<dyn Error + Send + Sync>> {
            if !from_addresses.is_empty() && !from_addresses.contains(&&utxo.address) {
                return Ok(true);
            }
            if utxo_manager.is_utxo_pending(&utxo, dag_info.virtual_daa_score) {
                return Ok(true);
            }

            {
                if let Some(broadcast_time) =
                    transaction_generator.used_outpoints.get(&utxo.outpoint)
                {
                    if allowed_used_outpoints.contains(&utxo.outpoint) {
                        if transaction_generator.has_used_outpoint_expired(
                            &start_time_of_last_completed_refresh,
                            broadcast_time,
                        ) {
                            transaction_generator.used_outpoints.remove(&utxo.outpoint);
                        }
                    } else {
                        return Ok(true);
                    }
                }
            }
            if avoid_preselected {
                if preselected_utxos.contains_key(&utxo.outpoint) {
                    return Ok(true);
                }
            }

            selected_utxos.push(utxo.clone());
            total_value += utxo.utxo_entry.amount;
            let estimated_recipient_value = if is_send_all { total_value } else { amount };
            fee = transaction_generator
                .estimate_fee(
                    &selected_utxos,
                    fee_rate,
                    max_fee,
                    estimated_recipient_value,
                    payload,
                )
                .await?;

            let total_spend = amount + fee;
            // Two break cases (if not send all):
            // 		1. total_value == totalSpend, so there's no change needed -> number of outputs = 1, so a single input is sufficient
            // 		2. total_value > totalSpend, so there will be change and 2 outputs, therefor in order to not struggle with --
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
            return Ok(true);
        };
        let utxos_sorted_by_amount: &Vec<WalletUtxo>;
        {
            let utxo_manager_mutex = self.utxo_manager.clone();
            let utxo_manager = utxo_manager_mutex.lock().await;

            let mut should_continue = true;
            for (_, preselected_utxo) in preselected_utxos {
                should_continue = iteration(self, &utxo_manager, preselected_utxo, false).await?;
                if !should_continue {
                    break;
                };
            }
            if should_continue {
                utxos_sorted_by_amount = utxo_manager.utxos_sorted_by_amount();
                for utxo in utxos_sorted_by_amount {
                    should_continue = iteration(self, &utxo_manager, utxo, true).await?;
                    if !should_continue {
                        break;
                    }
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
            return Err(Box::new(WalletError::UserInputError(format!(
                "Insufficient funds for send: {} required, while only {} available",
                amount / SOMPI_PER_KASPA,
                total_value / SOMPI_PER_KASPA
            ))));
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

    fn has_used_outpoint_expired(
        &self,
        start_time_of_last_completed_refresh: &DateTime<Utc>,
        outpoint_broadcast_time: &DateTime<Utc>,
    ) -> bool {
        // If the node returns a UTXO we previously attempted to spend and enough time has passed, we assume
        // that the network rejected or lost the previous transaction and allow a reuse. We set this time
        // interval to a minute.
        // We also verify that a full refresh UTXO operation started after this time point and has already
        // completed, in order to make sure that indeed this state reflects a state obtained following the required wait time.
        start_time_of_last_completed_refresh.gt(&outpoint_broadcast_time.add(Duration::minutes(1)))
    }

    async fn estimate_fee(
        &self,
        selected_utxos: &Vec<WalletUtxo>,
        fee_rate: f64,
        max_fee: u64,
        estimated_recipient_value: u64,
        payload: &Vec<u8>,
    ) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let estimated_mass = self
            .estimate_mass(selected_utxos, estimated_recipient_value, payload)
            .await?;
        let calculated_fee = ((estimated_mass as f64) * (fee_rate)).ceil() as u64;
        let fee = min(calculated_fee, max_fee);
        Ok(fee)
    }

    async fn estimate_mass(
        &self,
        selected_utxos: &Vec<WalletUtxo>,
        estimated_recipient_value: u64,
        payload: &Vec<u8>,
    ) -> Result<u64, Box<dyn Error + Send + Sync>> {
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
            .generate_unsigned_transaction(mock_payments, selected_utxos, payload.clone())
            .await?;

        Ok(self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
                &mock_transaction.transaction.unwrap_ref().tx,
                self.keys.minimum_signatures,
            ))
    }

    pub async fn estimate_mass_per_input(&self, input: &TransactionInput) -> u64 {
        self.mass_calculator
            .calc_compute_mass_for_client_transaction_input(&input)
            + self.signature_mass_per_input
    }

    pub async fn cleanup_expired_used_outpoints(&mut self) {
        let utxo_manager = self.utxo_manager.lock().await;
        let start_time_of_last_completed_refresh =
            utxo_manager.start_time_of_last_completed_refresh();
        // Cleanup expired used outpoints to avoid a memory leak
        for (outpoint, broadcast_time) in self.used_outpoints.clone() {
            if self
                .has_used_outpoint_expired(&start_time_of_last_completed_refresh, &broadcast_time)
            {
                self.used_outpoints.remove(&outpoint);
            }
        }
    }
}
