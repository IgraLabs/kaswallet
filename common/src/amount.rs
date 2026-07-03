use kaspa_consensus_core::constants::SOMPI_PER_KASPA;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::str::FromStr;

/// Format sompi amount as KAS with 8 decimal places, right-aligned in 19 characters.
pub fn format_kas(amount: u64) -> String {
    if amount == 0 {
        " ".repeat(19)
    } else {
        format!("{:>19.8}", amount as f64 / SOMPI_PER_KASPA as f64)
    }
}

/// Compact KAS rendering for logs and inline messages: no padding, and zero
/// renders as "0" (unlike `format_kas`, whose 19-char column form renders
/// zero as spaces).
pub fn kas_display(amount: u64) -> String {
    if amount == 0 {
        "0".to_string()
    } else {
        format_kas(amount).trim().to_string()
    }
}

/// Parse a KAS amount string into sompi.
/// Accepts formats like "1234" or "1234.12345678"
pub fn kas_to_sompi(amount: &str) -> Result<u64, String> {
    // Validate format: either an integer or a float with max 8 decimal places
    let re = regex::Regex::new(r"^([1-9]\d{0,11}|0)(\.\d{0,8})?$").unwrap();
    if !re.is_match(amount) {
        return Err("Invalid amount format".to_string());
    }
    let decimal = Decimal::from_str(amount).map_err(|e| format!("Invalid decimal: {}", e))?;
    let sompi = decimal * Decimal::from(SOMPI_PER_KASPA);
    sompi
        .to_u64()
        .ok_or_else(|| "Amount out of range for u64".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kas_to_sompi() {
        assert_eq!(kas_to_sompi("1").unwrap(), 100_000_000);
        assert_eq!(kas_to_sompi("1.0").unwrap(), 100_000_000);
        assert_eq!(kas_to_sompi("1.5").unwrap(), 150_000_000);
        assert_eq!(kas_to_sompi("0.00000001").unwrap(), 1);
        assert_eq!(kas_to_sompi("123.45678901").unwrap(), 12_345_678_901);
        assert_eq!(kas_to_sompi("0").unwrap(), 0);
    }

    #[test]
    fn test_kas_display() {
        assert_eq!(kas_display(0), "0");
        assert_eq!(kas_display(150_000_000), "1.50000000");
    }

    #[test]
    fn test_kas_to_sompi_invalid() {
        assert!(kas_to_sompi("abc").is_err());
        assert!(kas_to_sompi("-1").is_err());
        assert!(kas_to_sompi("1.123456789").is_err()); // Too many decimals
    }
}
