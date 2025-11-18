use crate::errors::{ResultExt, WalletResult};
use crate::model::WalletSignableTransaction;
use tonic::codegen::Bytes;

// TODO: Use protobuf instead of borsh for serialization

pub fn decode_transaction(
    encoded_transaction: &Bytes,
) -> WalletResult<WalletSignableTransaction> {
    let unsigned_transaction =
        borsh::from_slice(&encoded_transaction).to_wallet_result_user_input()?;
    Ok(unsigned_transaction)
}

pub fn decode_transactions(
    encoded_transactions: &Vec<Bytes>,
) -> WalletResult<Vec<WalletSignableTransaction>> {
    let mut decoded_transactions = vec![];
    for encoded_transaction in encoded_transactions {
        let unsigned_transaction = decode_transaction(encoded_transaction)?;
        decoded_transactions.push(unsigned_transaction);
    }
    Ok(decoded_transactions)
}

pub fn encode_transaction(transaction: &WalletSignableTransaction) -> WalletResult<Bytes> {
    let encoded_transaction = borsh::to_vec(transaction).to_wallet_result_internal()?;
    Ok(encoded_transaction.into())
}

pub fn encode_transactions(
    transactions: &Vec<WalletSignableTransaction>,
) -> WalletResult<Vec<Bytes>> {
    let mut encoded_transactions = Vec::with_capacity(transactions.len());
    for transaction in transactions {
        let encoded_transaction = encode_transaction(&transaction)?;
        encoded_transactions.push(encoded_transaction);
    }
    Ok(encoded_transactions)
}
