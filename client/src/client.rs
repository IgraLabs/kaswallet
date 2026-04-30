use crate::model::{AddressUtxos, BalanceInfo, SendResult};
use common::error_location::ErrorLocation;
use common::errors::{UserInputError, WalletError, WalletResult};
use common::model::WalletSignableTransaction;
use common::status_classify::{classify_rpc_status, classify_submit_status, classify_transport};
use kaspa_hashes::Hash;
use proto::kaswallet_proto::wallet_client::WalletClient as GrpcWalletClient;
use proto::kaswallet_proto::{
    BroadcastRequest, CreateUnsignedTransactionsRequest, GetAddressesRequest, GetBalanceRequest,
    GetUtxosRequest, GetVersionRequest, NewAddressRequest, SendRequest, SignRequest,
    TransactionDescription,
};
use std::str::FromStr;
use tonic::Request;
use tonic::transport::{Channel, Endpoint};

/// A convenient wrapper around the kaswallet gRPC client.
///
/// This client abstracts away the gRPC boilerplate and provides a clean,
/// ergonomic API for interacting with the kaswallet daemon.
#[derive(Clone)]
pub struct KaswalletClient {
    grpc_client: GrpcWalletClient<Channel>,
}

impl KaswalletClient {
    /// Connect to a kaswallet daemon at the specified address.
    ///
    /// # Arguments
    /// * `dst` — the address of the kaswallet daemon (e.g. `"http://localhost:8082"`).
    ///
    /// On failure the destination string is preserved in the typed error so
    /// operators can see *which* endpoint was unreachable. Earlier code threw
    /// it away when `try_into` consumed `dst`.
    ///
    /// # Example
    /// ```no_run
    /// # use kaswallet_client::client::KaswalletClient;
    /// # async fn example() -> common::errors::WalletResult<()> {
    /// let client = KaswalletClient::connect("http://localhost:8082").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(dst: &str) -> WalletResult<Self> {
        let endpoint = Endpoint::from_shared(dst.to_string()).map_err(|e| {
            WalletError::from(common::errors::RpcError::Connect {
                endpoint: dst.to_string(),
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })
        })?;
        let inner = GrpcWalletClient::connect(endpoint)
            .await
            .map_err(|e| WalletError::from(classify_transport(dst, e)))?;
        Ok(Self { grpc_client: inner })
    }

    /// Get the version of the kaswallet daemon.
    pub async fn get_version(&mut self) -> WalletResult<String> {
        let response = self
            .grpc_client
            .get_version(Request::new(GetVersionRequest {}))
            .await
            .map_err(|s| WalletError::from(classify_rpc_status("get_version", s)))?
            .into_inner();
        Ok(response.version)
    }

    /// Get all addresses in the wallet.
    pub async fn get_addresses(&mut self) -> WalletResult<Vec<String>> {
        let response = self
            .grpc_client
            .get_addresses(Request::new(GetAddressesRequest {}))
            .await
            .map_err(|s| WalletError::from(classify_rpc_status("get_addresses", s)))?
            .into_inner();
        Ok(response.address)
    }

    /// Generate a new address in the wallet.
    pub async fn new_address(&mut self) -> WalletResult<String> {
        let response = self
            .grpc_client
            .new_address(Request::new(NewAddressRequest {}))
            .await
            .map_err(|s| WalletError::from(classify_rpc_status("new_address", s)))?
            .into_inner();
        Ok(response.address)
    }

    /// Get the balance of the wallet.
    pub async fn get_balance(
        &mut self,
        include_balance_per_address: bool,
    ) -> WalletResult<BalanceInfo> {
        let response = self
            .grpc_client
            .get_balance(Request::new(GetBalanceRequest {
                include_balance_per_address,
            }))
            .await
            .map_err(|s| WalletError::from(classify_rpc_status("get_balance", s)))?
            .into_inner();

        Ok(BalanceInfo {
            available: response.available,
            pending: response.pending,
            address_balances: response
                .address_balances
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }

    /// Get UTXOs for the wallet.
    pub async fn get_utxos(
        &mut self,
        addresses: Vec<String>,
        include_pending: bool,
        include_dust: bool,
    ) -> WalletResult<Vec<AddressUtxos>> {
        let response = self
            .grpc_client
            .get_utxos(Request::new(GetUtxosRequest {
                addresses,
                include_pending,
                include_dust,
            }))
            .await
            .map_err(|s| WalletError::from(classify_rpc_status("get_utxos", s)))?
            .into_inner();

        Ok(response
            .addresses_to_utxos
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Create unsigned transactions based on the transaction description.
    pub async fn create_unsigned_transactions(
        &mut self,
        transaction_description: TransactionDescription,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        let response = self
            .grpc_client
            .create_unsigned_transactions(Request::new(CreateUnsignedTransactionsRequest {
                transaction_description: Some(transaction_description),
            }))
            .await
            .map_err(|s| WalletError::from(classify_rpc_status("create_unsigned_transactions", s)))?
            .into_inner();

        Ok(response
            .unsigned_transactions
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Sign unsigned transactions with the wallet's private keys.
    pub async fn sign(
        &mut self,
        unsigned_transactions: Vec<WalletSignableTransaction>,
        password: String,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        let response = self
            .grpc_client
            .sign(Request::new(SignRequest {
                unsigned_transactions: unsigned_transactions.into_iter().map(Into::into).collect(),
                password,
            }))
            .await
            .map_err(|s| WalletError::from(classify_rpc_status("sign", s)))?
            .into_inner();

        Ok(response
            .signed_transactions
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Broadcast signed transactions to the network.
    pub async fn broadcast(
        &mut self,
        transactions: Vec<WalletSignableTransaction>,
    ) -> WalletResult<Vec<Hash>> {
        let response = self
            .grpc_client
            .broadcast(Request::new(BroadcastRequest {
                transactions: transactions.into_iter().map(Into::into).collect(),
            }))
            .await
            .map_err(|s| {
                // No single tx_id at broadcast level; treat as generic RPC failure.
                WalletError::from(classify_rpc_status("broadcast", s))
            })?
            .into_inner();

        Self::transaction_ids_to_hashes(response.transaction_ids)
    }

    /// Send funds in a single operation (create, sign, and broadcast).
    pub async fn send(
        &mut self,
        transaction_description: TransactionDescription,
        password: String,
    ) -> WalletResult<SendResult> {
        let response = self
            .grpc_client
            .send(Request::new(SendRequest {
                transaction_description: Some(transaction_description),
                password,
            }))
            .await
            .map_err(|s| {
                // For send, use submit classification keyed by a placeholder tx_id — the
                // server has the real tx_id, but if the status conveys a mempool rejection
                // we want to surface that as TransactionError::Rejected.
                WalletError::from(classify_submit_status(Hash::default(), s))
            })?
            .into_inner();

        let transaction_ids = Self::transaction_ids_to_hashes(response.transaction_ids)?;

        Ok(SendResult {
            transaction_ids,
            signed_transactions: response
                .signed_transactions
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }

    fn transaction_ids_to_hashes(transaction_ids: Vec<String>) -> WalletResult<Vec<Hash>> {
        transaction_ids
            .into_iter()
            .map(|id| {
                Hash::from_str(&id).map_err(|_| {
                    WalletError::from(UserInputError::InvalidTransactionId {
                        input: id,
                        location: ErrorLocation::capture(),
                    })
                })
            })
            .collect()
    }
}
