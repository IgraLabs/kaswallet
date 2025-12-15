use kaspa_consensus_core::constants::SOMPI_PER_KASPA;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for ParseError {}

/// Format sompi amount as KAS with 8 decimal places, right-aligned in 19 characters.
pub fn format_kas(amount: u64) -> String {
    if amount == 0 {
        "                   ".to_string()
    } else {
        format!("{:>19.8}", amount as f64 / SOMPI_PER_KASPA as f64)
    }
}

/// Parse a KAS amount string into sompi.
/// Accepts formats like "1234" or "1234.12345678"
pub fn kas_to_sompi(amount: &str) -> Result<u64, ParseError> {
    // Validate format: either an integer or a float with max 8 decimal places
    let re = regex::Regex::new(r"^([1-9]\d{0,11}|0)(\.\d{0,8})?$").unwrap();
    if !re.is_match(amount) {
        return Err(ParseError("Invalid amount format".to_string()));
    }

    let parts: Vec<&str> = amount.split('.').collect();
    let integer_part = parts[0];
    let decimal_part = if parts.len() > 1 { parts[1] } else { "" };

    // Pad decimal part to 8 digits
    let decimal_padded = format!("{:0<8}", decimal_part);

    // Combine and parse
    let combined = format!("{}{}", integer_part, decimal_padded);
    combined
        .parse::<u64>()
        .map_err(|e| ParseError(format!("Failed to parse amount: {}", e)))
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
    fn test_kas_to_sompi_invalid() {
        assert!(kas_to_sompi("abc").is_err());
        assert!(kas_to_sompi("-1").is_err());
        assert!(kas_to_sompi("1.123456789").is_err()); // Too many decimals
    }
}
