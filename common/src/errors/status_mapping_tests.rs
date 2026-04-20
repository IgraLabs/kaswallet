use super::*;
use crate::error_location::ErrorLocation;
use tonic::Code;

fn loc() -> ErrorLocation {
    ErrorLocation::capture()
}

#[test]
fn to_status_mapping_table() {
    let cases: Vec<(WalletError, Code)> = vec![
        (
            UserInputError::MissingField {
                field: "f",
                loc: loc(),
            }
            .into(),
            Code::InvalidArgument,
        ),
        (
            ConfigError::MissingArgument {
                name: "x",
                loc: loc(),
            }
            .into(),
            Code::FailedPrecondition,
        ),
        (
            CryptoError::KeyFileNotFound {
                path: "/k".into(),
                loc: loc(),
            }
            .into(),
            Code::Internal,
        ),
        (
            RpcError::Transport {
                reason: "closed".into(),
                loc: loc(),
            }
            .into(),
            Code::Unavailable,
        ),
        (
            StorageError::Io {
                path: "/x".into(),
                reason: "e".into(),
                loc: loc(),
            }
            .into(),
            Code::Internal,
        ),
        (
            SyncError::UtxoIndexInconsistent {
                reason: "e".into(),
                loc: loc(),
            }
            .into(),
            Code::Internal,
        ),
        (
            TransactionError::InsufficientFunds {
                required_sompi: 1,
                available_sompi: 0,
                loc: loc(),
            }
            .into(),
            Code::InvalidArgument,
        ),
        (
            TransactionError::FeeTooLow {
                provided_sompi: 0,
                required_sompi: 1,
                loc: loc(),
            }
            .into(),
            Code::InvalidArgument,
        ),
        (
            TransactionError::InvalidSignature {
                input_index: 0,
                loc: loc(),
            }
            .into(),
            Code::InvalidArgument,
        ),
        (
            TransactionError::BuildFailed {
                reason: "r".into(),
                loc: loc(),
            }
            .into(),
            Code::Internal,
        ),
        (
            TransactionError::SerializationFailed {
                stage: "s",
                reason: "r".into(),
                loc: loc(),
            }
            .into(),
            Code::Internal,
        ),
        (
            TransactionError::SignFailed {
                input_index: 0,
                reason: "r".into(),
                loc: loc(),
            }
            .into(),
            Code::Internal,
        ),
        (
            TransactionError::MassExceeded {
                mass: 1,
                limit: 0,
                loc: loc(),
            }
            .into(),
            Code::Internal,
        ),
    ];
    for (err, expected_code) in cases {
        let status = err.clone().to_status();
        assert_eq!(status.code(), expected_code, "wrong code for {err}");
        assert!(!status.message().is_empty(), "empty message for {err}");
    }
}

#[test]
fn rejected_maps_to_aborted() {
    let err: WalletError = TransactionError::Rejected {
        tx_id: kaspa_hashes::Hash::from_bytes([0; 32]),
        node_message: "mempool full".into(),
        loc: loc(),
    }
    .into();
    let status = err.to_status();
    assert_eq!(status.code(), Code::Aborted);
}

#[test]
fn orphan_maps_to_aborted() {
    let err: WalletError = TransactionError::Orphan {
        tx_id: kaspa_hashes::Hash::from_bytes([0; 32]),
        loc: loc(),
    }
    .into();
    assert_eq!(err.to_status().code(), Code::Aborted);
}

#[test]
fn double_spend_maps_to_invalid_argument() {
    use kaspa_consensus_core::tx::TransactionOutpoint;
    let err: WalletError = TransactionError::DoubleSpend {
        tx_id: kaspa_hashes::Hash::from_bytes([0; 32]),
        conflicting_outpoint: TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([1; 32]), 0),
        loc: loc(),
    }
    .into();
    assert_eq!(err.to_status().code(), Code::InvalidArgument);
}
