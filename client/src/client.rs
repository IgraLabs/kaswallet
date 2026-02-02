use crate::model::{AddressUtxos, BalanceInfo, Result, SendResult};
use common::model::WalletSignableTransaction;
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
    /// * `addr` - The address of the kaswallet daemon (e.g., "http://localhost:8082")
    ///
    /// # Example
    /// ```no_run
    /// # use kaswallet_client::client::KaswalletClient;
    /// # use kaswallet_client::model::Result;
    /// # async fn example() -> Result<()> {
    /// let client = KaswalletClient::connect("http://localhost:8082").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect<D>(dst: D) -> Result<Self>
    where
        D: TryInto<Endpoint>,
        D::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let inner = GrpcWalletClient::connect(dst).await?;
        Ok(Self { grpc_client: inner })
    }

    /// Get the version of the kaswallet daemon.
    pub async fn get_version(&mut self) -> Result<String> {
        let response = self
            .grpc_client
            .get_version(Request::new(GetVersionRequest {}))
            .await?
            .into_inner();
        Ok(response.version)
    }

    /// Get all addresses in the wallet.
    pub async fn get_addresses(&mut self) -> Result<Vec<String>> {
        let response = self
            .grpc_client
            .get_addresses(Request::new(GetAddressesRequest {}))
            .await?
            .into_inner();
        Ok(response.address)
    }

    /// Generate a new address in the wallet.
    pub async fn new_address(&mut self) -> Result<String> {
        let response = self
            .grpc_client
            .new_address(Request::new(NewAddressRequest {}))
            .await?
            .into_inner();
        Ok(response.address)
    }

    /// Get the balance of the wallet.
    ///
    /// # Arguments
    /// * `include_balance_per_address` - If true, includes balance breakdown per address
    pub async fn get_balance(&mut self, include_balance_per_address: bool) -> Result<BalanceInfo> {
        let response = self
            .grpc_client
            .get_balance(Request::new(GetBalanceRequest {
                include_balance_per_address,
            }))
            .await?
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
    ///
    /// # Arguments
    /// * `addresses` - Optional list of addresses to filter UTXOs. If empty, returns all UTXOs.
    /// * `include_pending` - If true, includes pending coinbase UTXOs
    /// * `include_dust` - If true, includes UTXOs whose value is less than the fee to spend them
    pub async fn get_utxos(
        &mut self,
        addresses: Vec<String>,
        include_pending: bool,
        include_dust: bool,
    ) -> Result<Vec<AddressUtxos>> {
        let response = self
            .grpc_client
            .get_utxos(Request::new(GetUtxosRequest {
                addresses,
                include_pending,
                include_dust,
            }))
            .await?
            .into_inner();

        Ok(response
            .addresses_to_utxos
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Create unsigned transactions based on the transaction description.
    ///
    /// # Arguments
    /// * transaction_description - description of requested transaction:
    ///   * `to_address` - Destination address
    ///   * `amount` - Amount to send (mutually exclusive with is_send_all)
    ///   * `is_send_all` - If true, sends all available funds (mutually exclusive with amount)
    ///   * `payload` - Optional transaction payload
    ///   * `from_addresses` - Optional list of source addresses to spend from
    ///   * `utxos` - Optional list of specific UTXOs to spend (mutually exclusive with from_addresses)
    ///   * `use_existing_change_address` - If true, uses existing change address instead of generating new one
    ///   * `fee_policy` - Optional fee policy for the transaction
    /// * `password` - The wallet password
    pub async fn create_unsigned_transactions(
        &mut self,
        transaction_description: TransactionDescription,
    ) -> Result<Vec<WalletSignableTransaction>> {
        let response = self
            .grpc_client
            .create_unsigned_transactions(Request::new(CreateUnsignedTransactionsRequest {
                transaction_description: Some(transaction_description),
            }))
            .await?
            .into_inner();

        Ok(response
            .unsigned_transactions
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Sign unsigned transactions with the wallet's private keys.
    ///
    /// # Arguments
    /// * `unsigned_transactions` - The transactions to sign
    /// * `password` - The wallet password
    ///
    /// # Security Note
    /// This command sends the password over the network. Only use on trusted or secure connections.
    pub async fn sign(
        &mut self,
        unsigned_transactions: Vec<WalletSignableTransaction>,
        password: String,
    ) -> Result<Vec<WalletSignableTransaction>> {
        let response = self
            .grpc_client
            .sign(Request::new(SignRequest {
                unsigned_transactions: unsigned_transactions.into_iter().map(Into::into).collect(),
                password,
            }))
            .await?
            .into_inner();

        Ok(response
            .signed_transactions
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Broadcast signed transactions to the network.
    ///
    /// # Arguments
    /// * `transactions` - The signed transactions to broadcast
    ///
    /// # Returns
    /// * `Vec<Hash>` - List of transaction IDs
    pub async fn broadcast(
        &mut self,
        transactions: Vec<WalletSignableTransaction>,
    ) -> Result<Vec<Hash>> {
        let response = self
            .grpc_client
            .broadcast(Request::new(BroadcastRequest {
                transactions: transactions.into_iter().map(Into::into).collect(),
            }))
            .await?
            .into_inner();

        Self::transaction_ids_to_hashes(response.transaction_ids)
    }

    /// Send funds in a single operation (create, sign, and broadcast).
    ///
    /// # Arguments
    /// * transaction_description - description of requested transaction:
    ///   * `to_address` - Destination address
    ///   * `amount` - Amount to send (mutually exclusive with is_send_all)
    ///   * `is_send_all` - If true, sends all available funds (mutually exclusive with amount)
    ///   * `payload` - Optional transaction payload
    ///   * `from_addresses` - Optional list of source addresses to spend from
    ///   * `utxos` - Optional list of specific UTXOs to spend (mutually exclusive with from_addresses)
    ///   * `use_existing_change_address` - If true, uses existing change address instead of generating new one
    ///   * `fee_policy` - Optional fee policy for the transaction
    /// * `password` - The wallet password
    ///
    /// # Security Note
    /// This command sends the password over the network. Only use on trusted or secure connections.
    pub async fn send(
        &mut self,
        transaction_description: TransactionDescription,
        password: String,
    ) -> Result<SendResult> {
        let response = self
            .grpc_client
            .send(Request::new(SendRequest {
                transaction_description: Some(transaction_description),
                password,
            }))
            .await?
            .into_inner();

        let transaction_ids: Result<Vec<Hash>> =
            Self::transaction_ids_to_hashes(response.transaction_ids);

        Ok(SendResult {
            transaction_ids: transaction_ids?,
            signed_transactions: response
                .signed_transactions
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }

    #[allow(clippy::result_large_err)]
    fn transaction_ids_to_hashes(transaction_ids: Vec<String>) -> Result<Vec<Hash>> {
        transaction_ids
            .into_iter()
            .map(|id| {
                Hash::from_str(&id).map_err(|_| crate::model::ClientError::InvalidTransactionId(id))
            })
            .collect()
    }
}
