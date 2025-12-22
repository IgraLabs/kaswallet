use crate::client::KaswalletClient;
use common::model::WalletSignableTransaction;
use kaspa_hashes::Hash;
use proto::kaswallet_proto::{
    AddressBalances as ProtoAddressBalances, AddressToUtxos as ProtoAddressToUtxos, FeePolicy,
    Outpoint, TransactionDescription, Utxo as ProtoUtxo,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("gRPC status error: {0}")]
    Status(#[from] tonic::Status),
    #[error("Invalid transaction ID: {0}")]
    InvalidTransactionId(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// Balance information for a specific address.
#[derive(Debug, Clone)]
pub struct AddressBalance {
    pub address: String,
    pub available: u64,
    pub pending: u64,
}

impl From<ProtoAddressBalances> for AddressBalance {
    fn from(value: ProtoAddressBalances) -> Self {
        Self {
            address: value.address,
            available: value.available,
            pending: value.pending,
        }
    }
}

/// Overall wallet balance information.
#[derive(Debug, Clone)]
pub struct BalanceInfo {
    pub available: u64,
    pub pending: u64,
    pub address_balances: Vec<AddressBalance>,
}

/// UTXO information.
#[derive(Debug, Clone)]
pub struct Utxo {
    pub outpoint: Outpoint,
    pub amount: u64,
    pub script_public_key_version: u32,
    pub script_public_key: String,
    pub block_daa_score: u64,
    pub is_coinbase: bool,
    pub is_pending: bool,
    pub is_dust: bool,
}

impl From<ProtoUtxo> for Utxo {
    fn from(value: ProtoUtxo) -> Self {
        let outpoint = value.outpoint.unwrap_or_default();
        let utxo_entry = value.utxo_entry.unwrap_or_default();
        let script_public_key = utxo_entry.script_public_key.unwrap_or_default();

        Self {
            outpoint,
            amount: utxo_entry.amount,
            script_public_key_version: script_public_key.version,
            script_public_key: script_public_key.script_public_key,
            block_daa_score: utxo_entry.block_daa_score,
            is_coinbase: utxo_entry.is_coinbase,
            is_pending: value.is_pending,
            is_dust: value.is_dust,
        }
    }
}

/// UTXOs grouped by address.
#[derive(Debug, Clone)]
pub struct AddressUtxos {
    pub address: String,
    pub utxos: Vec<Utxo>,
}

impl From<ProtoAddressToUtxos> for AddressUtxos {
    fn from(value: ProtoAddressToUtxos) -> Self {
        Self {
            address: value.address,
            utxos: value.utxos.into_iter().map(Into::into).collect(),
        }
    }
}

/// Result of a send operation.
#[derive(Debug, Clone)]
pub struct SendResult {
    pub transaction_ids: Vec<Hash>,
    pub signed_transactions: Vec<WalletSignableTransaction>,
}

/// Builder pattern for transaction operations with a more ergonomic API.
///
/// This builder can be used for both creating unsigned transactions and
/// for the full send operation (create, sign, and broadcast).
pub struct TransactionBuilder {
    to_address: String,
    amount: Option<u64>,
    is_send_all: bool,
    payload: Vec<u8>,
    from_addresses: Vec<String>,
    utxos: Vec<Outpoint>,
    use_existing_change_address: bool,
    fee_policy: Option<FeePolicy>,
}

impl TransactionBuilder {
    /// Create a new transaction builder with the destination address.
    pub fn new(to_address: String) -> Self {
        Self {
            to_address,
            amount: None,
            is_send_all: false,
            payload: Vec::new(),
            from_addresses: Vec::new(),
            utxos: Vec::new(),
            use_existing_change_address: false,
            fee_policy: None,
        }
    }

    /// Set the amount to send (mutually exclusive with send_all).
    pub fn amount(mut self, amount: u64) -> Self {
        self.amount = Some(amount);
        self.is_send_all = false;
        self
    }

    /// Set to send all available funds (mutually exclusive with amount).
    pub fn send_all(mut self) -> Self {
        self.is_send_all = true;
        self.amount = None;
        self
    }

    /// Set the transaction payload.
    pub fn payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    /// Set the source addresses to spend from.
    pub fn from_addresses(mut self, addresses: Vec<String>) -> Self {
        self.from_addresses = addresses;
        self
    }

    /// Set specific UTXOs to spend.
    pub fn utxos(mut self, utxos: Vec<Outpoint>) -> Self {
        self.utxos = utxos;
        self
    }

    /// Use existing change address instead of generating a new one.
    pub fn use_existing_change_address(mut self, use_existing: bool) -> Self {
        self.use_existing_change_address = use_existing;
        self
    }

    /// Set the fee policy.
    pub fn fee_policy(mut self, fee_policy: FeePolicy) -> Self {
        self.fee_policy = Some(fee_policy);
        self
    }

    pub fn transaction_description(&self) -> TransactionDescription {
        TransactionDescription {
            to_address: self.to_address.clone(),
            amount: self.amount.unwrap_or(0),
            is_send_all: self.is_send_all,
            payload: self.payload.clone().into(),
            from_addresses: self.from_addresses.clone(),
            utxos: self.utxos.clone(),
            use_existing_change_address: self.use_existing_change_address,
            fee_policy: self.fee_policy,
        }
    }

    /// Create unsigned transactions without signing or broadcasting.
    pub async fn create_unsigned_transactions(
        &self,
        client: &mut KaswalletClient,
    ) -> Result<Vec<WalletSignableTransaction>> {
        client
            .create_unsigned_transactions(self.transaction_description())
            .await
    }

    /// Execute the full send operation (create, sign, and broadcast).
    ///
    /// # Security Note
    /// This command sends the password over the network. Only use on trusted or secure connections.
    pub async fn send(self, client: &mut KaswalletClient, password: String) -> Result<SendResult> {
        client.send(self.transaction_description(), password).await
    }
}
