use common::error_location::ErrorLocation;
use common::errors::{StorageError, UserInputError, WalletError, WalletResult};
use common::model::WalletPayment;
use kaspa_addresses::{Address, Prefix};
use std::collections::BTreeMap;

/// Parse repeated `--output <address>:<amount-KAS>` values into payments,
/// validating each address against the selected network's prefix.
pub fn parse_outputs(
    raw_outputs: &[String],
    expected_prefix: Prefix,
) -> WalletResult<Vec<WalletPayment>> {
    raw_outputs
        .iter()
        .map(|raw| parse_output(raw, expected_prefix))
        .collect()
}

/// Parse a JSON outputs file: a map of `"address": "amount-KAS"` STRING
/// entries, e.g. `{"kaspatest:qq...": "1.5"}`. Amounts must be JSON strings
/// — JSON numbers are floats and cannot represent all 8-decimal KAS values
/// exactly. Iteration is address-sorted (`BTreeMap`), so output order is
/// deterministic. A map cannot express duplicate recipient addresses; use
/// repeated `--output` pairs for that.
pub fn parse_outputs_file(path: &str, expected_prefix: Prefix) -> WalletResult<Vec<WalletPayment>> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        WalletError::from(StorageError::Io {
            path: path.to_string(),
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })
    })?;
    let entries: BTreeMap<String, String> = serde_json::from_str(&content).map_err(|e| {
        WalletError::from(UserInputError::InvalidArgument {
            reason: format!(
                "outputs file {path:?} must be a JSON map of \"address\": \"amount-KAS\" \
                 string entries (amounts must be strings, not numbers): {e}"
            ),
            location: ErrorLocation::capture(),
        })
    })?;
    if entries.is_empty() {
        return Err(WalletError::from(UserInputError::InvalidArgument {
            reason: format!("outputs file {path:?} contains no recipients"),
            location: ErrorLocation::capture(),
        }));
    }
    entries
        .iter()
        .map(|(address_str, amount_str)| {
            validated_payment(
                address_str,
                amount_str,
                &format!("outputs file entry {address_str:?}"),
                expected_prefix,
            )
        })
        .collect()
}

fn parse_output(raw: &str, expected_prefix: Prefix) -> WalletResult<WalletPayment> {
    // Kaspa addresses embed a prefix colon (`kaspatest:qq...`), so the
    // amount separator is the LAST colon.
    let Some((address_str, amount_str)) = raw.rsplit_once(':') else {
        return Err(WalletError::from(UserInputError::InvalidArgument {
            reason: format!("--output must be <address>:<amount-KAS>, got {raw:?}"),
            location: ErrorLocation::capture(),
        }));
    };
    validated_payment(
        address_str,
        amount_str,
        &format!("--output {raw:?}"),
        expected_prefix,
    )
}

/// Shared validation for one recipient regardless of source: well-formed
/// address, right network, positive exactly-representable KAS amount.
/// `context` names the offending input in error messages.
fn validated_payment(
    address_str: &str,
    amount_str: &str,
    context: &str,
    expected_prefix: Prefix,
) -> WalletResult<WalletPayment> {
    let address = Address::try_from(address_str).map_err(|e| {
        WalletError::from(UserInputError::InvalidAddress {
            input: address_str.to_string(),
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })
    })?;
    // `Address::try_from` accepts any well-formed address regardless of
    // network, and script construction ignores the prefix — without this
    // check a wrong-network recipient would be paid silently.
    if address.prefix != expected_prefix {
        return Err(WalletError::from(UserInputError::InvalidAddress {
            input: address_str.to_string(),
            reason: format!(
                "address prefix {} does not match the selected network (expected {})",
                address.prefix, expected_prefix
            ),
            location: ErrorLocation::capture(),
        }));
    }

    let amount_sompi = common::amount::kas_to_sompi(amount_str).map_err(|e| {
        WalletError::from(UserInputError::InvalidArgument {
            reason: format!("invalid amount {amount_str:?} in {context}: {e}"),
            location: ErrorLocation::capture(),
        })
    })?;
    if amount_sompi == 0 {
        return Err(WalletError::from(UserInputError::InvalidArgument {
            reason: format!("amount must be greater than 0 in {context}"),
            location: ErrorLocation::capture(),
        }));
    }

    Ok(WalletPayment::new(address, amount_sompi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_addresses::Version;

    fn simnet_address(seed: u8) -> String {
        Address::new(Prefix::Simnet, Version::PubKey, &[seed; 32]).to_string()
    }

    #[test]
    fn parses_single_output() {
        let raw = vec![format!("{}:1.5", simnet_address(1))];
        let payments = parse_outputs(&raw, Prefix::Simnet).unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].amount, 150_000_000);
        assert_eq!(payments[0].address.prefix, Prefix::Simnet);
    }

    #[test]
    fn parses_multiple_outputs_and_allows_duplicates() {
        let address = simnet_address(2);
        let raw = vec![format!("{address}:1"), format!("{address}:2")];
        let payments = parse_outputs(&raw, Prefix::Simnet).unwrap();
        assert_eq!(payments.len(), 2);
        assert_eq!(payments[0].address, payments[1].address);
        assert_eq!(payments[0].amount, 100_000_000);
        assert_eq!(payments[1].amount, 200_000_000);
    }

    #[test]
    fn rejects_missing_separator() {
        let err = parse_outputs(&["no-colon-here".to_string()], Prefix::Simnet)
            .expect_err("missing separator must fail");
        assert!(err.to_string().contains("<address>:<amount-KAS>"));
    }

    #[test]
    fn rejects_malformed_address() {
        let err = parse_outputs(&["kaspasim:notanaddress:1".to_string()], Prefix::Simnet)
            .expect_err("bad address must fail");
        assert!(err.to_string().contains("InvalidAddress"));
    }

    #[test]
    fn rejects_wrong_network_prefix() {
        let testnet_address = Address::new(Prefix::Testnet, Version::PubKey, &[3; 32]).to_string();
        let err = parse_outputs(&[format!("{testnet_address}:1")], Prefix::Simnet)
            .expect_err("wrong-network address must fail");
        assert!(
            err.to_string()
                .contains("does not match the selected network")
        );
    }

    #[test]
    fn rejects_zero_and_malformed_amounts() {
        let address = simnet_address(4);
        for bad_amount in ["0", "abc", "1.123456789", ""] {
            let err = parse_outputs(&[format!("{address}:{bad_amount}")], Prefix::Simnet)
                .expect_err("bad amount must fail");
            assert!(
                err.to_string().contains("amount"),
                "unexpected error for {bad_amount:?}: {err}"
            );
        }
    }

    fn write_outputs_file(content: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        std::fs::write(file.path(), content).unwrap();
        file
    }

    #[test]
    fn file_parses_map_in_deterministic_order() {
        let address_a = simnet_address(1);
        let address_b = simnet_address(2);
        // Deliberately out of order in the file — BTreeMap sorts by address.
        let file = write_outputs_file(&format!(
            "{{\"{address_b}\": \"2\", \"{address_a}\": \"1.5\"}}"
        ));
        let payments = parse_outputs_file(file.path().to_str().unwrap(), Prefix::Simnet).unwrap();
        assert_eq!(payments.len(), 2);
        let mut sorted = [address_a.clone(), address_b.clone()];
        sorted.sort();
        assert_eq!(payments[0].address.to_string(), sorted[0]);
        assert_eq!(payments[1].address.to_string(), sorted[1]);
        let amount_of = |address: &str| {
            payments
                .iter()
                .find(|p| p.address.to_string() == address)
                .unwrap()
                .amount
        };
        assert_eq!(amount_of(&address_a), 150_000_000);
        assert_eq!(amount_of(&address_b), 200_000_000);
    }

    #[test]
    fn file_missing_is_a_storage_error() {
        let err = parse_outputs_file("/nonexistent/payouts.json", Prefix::Simnet)
            .expect_err("missing file must fail");
        assert!(err.to_string().contains("Io"), "got: {err}");
    }

    #[test]
    fn file_rejects_malformed_json_and_number_amounts() {
        for content in ["not json", "[1,2]", "{\"kaspasim:x\": 1.5}"] {
            let file = write_outputs_file(content);
            let err = parse_outputs_file(file.path().to_str().unwrap(), Prefix::Simnet)
                .expect_err("malformed file must fail");
            assert!(
                err.to_string().contains("JSON map"),
                "unexpected error for {content:?}: {err}"
            );
        }
    }

    #[test]
    fn file_rejects_empty_map() {
        let file = write_outputs_file("{}");
        let err = parse_outputs_file(file.path().to_str().unwrap(), Prefix::Simnet)
            .expect_err("empty map must fail");
        assert!(err.to_string().contains("no recipients"));
    }

    #[test]
    fn file_entries_get_full_validation() {
        // Zero amount.
        let address = simnet_address(5);
        let file = write_outputs_file(&format!("{{\"{address}\": \"0\"}}"));
        let err = parse_outputs_file(file.path().to_str().unwrap(), Prefix::Simnet)
            .expect_err("zero amount must fail");
        assert!(err.to_string().contains("greater than 0"));

        // Wrong network prefix.
        let testnet_address = Address::new(Prefix::Testnet, Version::PubKey, &[6; 32]).to_string();
        let file = write_outputs_file(&format!("{{\"{testnet_address}\": \"1\"}}"));
        let err = parse_outputs_file(file.path().to_str().unwrap(), Prefix::Simnet)
            .expect_err("wrong network must fail");
        assert!(
            err.to_string()
                .contains("does not match the selected network")
        );

        // Malformed address.
        let file = write_outputs_file("{\"kaspasim:notanaddress\": \"1\"}");
        let err = parse_outputs_file(file.path().to_str().unwrap(), Prefix::Simnet)
            .expect_err("bad address must fail");
        assert!(err.to_string().contains("InvalidAddress"));
    }
}
