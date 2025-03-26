use std::cmp::min;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::constants::SOMPI_PER_KASPA;
use kaspa_consensus_core::tx::{SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry};
use kaspa_txscript::pay_to_address_script;
use kaspa_wallet_core::tx::MassCalculator;
use wallet_proto::wallet_proto::{FeePolicy, Outpoint};
use crate::model::{UserInputError, WalletAddress, WalletPayment, WalletSignableTransaction, WalletUtxo};

pub struct TransactionGenerator{
    mass_calculator: MassCalculator,
}

impl TransactionGenerator {
    pub fn new() -> Self {
        let mass_calculator = MassCalculator::new(&network_id.network_type.into());
        Self{
            mass_calculator
        }
    }

    pub async fn create_unsigned_transactions(
        &self,
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
                    Err(e) => Err(Box::new(UserInputError::new(format!(
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
            return Err(Box::new(UserInputError::new(
                "Cannot specify both from_addresses and utxos".to_string(),
            )));
        }

        let from_addresses = if from_addresses_strings.is_empty() {
            vec![]
        } else {
            let mut from_addresses = vec![];
            for address_string in from_addresses_strings {
                let wallet_address = address_set.get(&address_string).ok_or_else(|| {
                    UserInputError::new(format!(
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
                let utxos_sorted_by_amount = self.utxos_sorted_by_amount.lock().await;
                for outpoint in preselected_utxo_outpoints {
                    // TODO: index utxos by outpoint instead of searching all over an array
                    if let Some(utxo) = utxos_sorted_by_amount.iter().find(|utxo| {
                        utxo.outpoint.transaction_id.to_string() == outpoint.transaction_id
                            && utxo.outpoint.index == outpoint.index
                    }) {
                        let utxo = utxo.clone();
                        preselected_utxos.insert(utxo.outpoint.clone(), utxo);
                    } else {
                        return Err(Box::new(UserInputError::new(format!(
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
                address_manager.change_address(use_existing_change_address, &from_addresses)?;
        }

        let (selected_utxos, amount_sent_to_recipient, change_sompi) = self
            .select_utxos(
                preselected_utxos,
                HashSet::new(),
                amount,
                is_send_all,
                fee_rate,
                max_fee,
                from_addresses,
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
            .generate_unsigned_transaction(payments, payload, &selected_utxos)
            .await?;

        let unsigned_transactions = self
            .maybe_auto_compound_transaction(
                unsigned_transaction,
                to_address,
                change_address,
                change_wallet_address,
                fee_rate,
                max_fee,
            )
            .await?;

        Ok(unsigned_transactions)
    }

    async fn maybe_auto_compound_transaction(
        &self,
        unsigned_transaction: WalletSignableTransaction,
        to_address: Address,
        change_address: Address,
        change_wallet_address: WalletAddress,
        fee_rate: f64,
        max_fee: u64,
    ) -> Result<Vec<WalletSignableTransaction>, Box<dyn Error + Send + Sync>> {
        // TODO: implement actual splitting of transactions
        Ok(vec![unsigned_transaction])
    }

    async fn generate_unsigned_transaction(
        &self,
        payments: Vec<WalletPayment>,
        payload: Vec<u8>,
        selected_utxos: &Vec<WalletUtxo>,
    ) -> Result<WalletSignableTransaction, Box<dyn Error + Send + Sync>> {
        let mut sorted_extended_public_keys = self.keys.public_keys.clone();
        sorted_extended_public_keys.sort();

        let mut inputs = vec![];
        let mut utxo_entries = vec![];
        let mut derivation_paths = HashSet::new();
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
        );

        Ok(wallet_signable_transaction)
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
            .generate_unsigned_transaction(mock_payments, payload.clone(), selected_utxos)
            .await?;

        Ok(self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
                &mock_transaction.transaction.unwrap().tx,
                self.keys.minimum_signatures,
            ))
    }

    async fn default_fee_rate(&self) -> Result<(f64, u64), Box<dyn Error + Send + Sync>> {
        let fee_estimate = self.kaspa_rpc_client.get_fee_estimate().await?;
        Ok((fee_estimate.normal_buckets[0].feerate, SOMPI_PER_KASPA)) // Default to a bound of max 1 KAS as fee
    }

}