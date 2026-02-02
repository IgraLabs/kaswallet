use crate::service::kaswallet_service::KasWalletService;
use common::errors::{ResultExt, WalletResult};
use log::info;
use proto::kaswallet_proto::{AddressBalances, GetBalanceRequest, GetBalanceResponse};
use std::collections::HashMap;

impl KasWalletService {
    pub(crate) async fn get_balance(
        &self,
        request: GetBalanceRequest,
    ) -> WalletResult<GetBalanceResponse> {
        self.check_is_synced().await?;

        let virtual_daa_score = self.get_virtual_daa_score().await?;
        let mut balances_map = HashMap::new();

        let utxos_count: usize;
        {
            let utxo_manager = self.utxo_manager.lock().await;
            utxos_count = utxo_manager.utxos_by_outpoint().len();
            for utxo in utxo_manager.utxos_by_outpoint().values() {
                let amount = utxo.utxo_entry.amount;
                let balances = balances_map
                    .entry(utxo.address.clone())
                    .or_insert_with(BalancesEntry::new);
                if utxo_manager.is_utxo_pending(utxo, virtual_daa_score) {
                    balances.add_pending(amount);
                } else {
                    balances.add_available(amount);
                }
            }
        }
        let mut address_balances = vec![];
        let mut total_balances = BalancesEntry::new();

        let address_manager = self.address_manager.lock().await;
        for (wallet_address, balances) in &balances_map {
            let address = address_manager
                .kaspa_address_from_wallet_address(wallet_address, true)
                .await
                .to_wallet_result_internal()?;

            if request.include_balance_per_address {
                address_balances.push(AddressBalances {
                    address: address.to_string(),
                    available: balances.available,
                    pending: balances.pending,
                });
            }
            total_balances.add(balances);
        }

        info!(
            "GetBalance request scanned {} UTXOs overall over {} addresses",
            utxos_count,
            balances_map.len()
        );

        Ok(GetBalanceResponse {
            available: total_balances.available,
            pending: total_balances.pending,
            address_balances,
        })
    }
}
#[derive(Clone)]
struct BalancesEntry {
    pub available: u64,
    pub pending: u64,
}

impl BalancesEntry {
    fn new() -> Self {
        Self {
            available: 0,
            pending: 0,
        }
    }

    pub fn add(&mut self, other: &Self) {
        self.add_available(other.available);
        self.add_pending(other.pending);
    }
    pub fn add_available(&mut self, amount: u64) {
        self.available += amount;
    }
    pub fn add_pending(&mut self, amount: u64) {
        self.pending += amount;
    }
}
