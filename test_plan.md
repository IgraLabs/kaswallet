# Kaswallet Test Plan

This document outlines the comprehensive test plan for porting tests from the Go kaspawallet to the Rust kaswallet implementation.

## Overview

The Rust kaswallet is a rewrite of the Go kaspawallet (excluding the client component). This test plan identifies all tests from the Go version that need to be ported to Rust, along with additional tests needed for Rust-specific functionality.

## Current Test Coverage in Go Kaspawallet

### 1. ~~Transaction Integration Tests~~ (`libkaspawallet/transaction_test.go`) - (DEFERRED)

**Note:** These tests validate complete transaction workflows including DAG acceptance, which requires a test consensus instance. Since the Rust wallet delegates to rusty-kaspa components for signing (kaspa-txscript, kaspa-bip32) and doesn't include consensus, these tests may need to be adapted or deferred.

#### TestMultisig (Integration)
End-to-end multisig transaction test:
- Creates 3 mnemonics and derives public keys (kaspa-bip32)
- Generates a multisig P2SH address (kaspa-addresses)
- Creates an unsigned transaction (wallet logic)
- Signs transaction incrementally (kaspa-txscript)
- Validates transaction in test consensus (requires consensus instance)

**Status:** DEFERRED - Requires test consensus setup or mocking strategy

#### TestP2PK (Integration)
End-to-end single-signature transaction test:
- Similar flow to TestMultisig but with single key
- Tests both Schnorr and ECDSA signature schemes

**Status:** DEFERRED - Requires test consensus setup or mocking strategy

#### TestMaxSompi (Integration)
Tests handling of large transaction amounts:
- Requires test consensus to create funding blocks
- Tests wallet's handling of large amounts (wallet logic is testable)

**Status:** DEFERRED - Requires test consensus setup, but wallet-specific amount handling can be unit tested separately

### 2. Transaction Splitting Tests (`daemon/server/split_transaction_test.go`)

**Note:** Mass calculation is done by `MassCalculator` from kaspa-wallet-core (a dependency), but the wallet implements estimation logic that predicts mass before signatures are added. This estimation logic is wallet-specific and should be tested.

#### TestEstimateComputeMassAfterSignatures
Tests wallet's estimation of transaction mass after signatures are added:
- Sets up multisig scenario (2-of-3)
- Creates unsigned transaction (wallet logic)
- Estimates mass after signatures using wallet's `estimateComputeMassAfterSignatures` (wallet logic - testable)
- Actually signs the transaction (kaspa-txscript)
- Compares estimated mass to actual mass (validates wallet estimation)

**Priority:** HIGH - Critical for fee estimation accuracy

**Status:** READY - Tests wallet-specific estimation logic

#### TestEstimateMassAfterSignatures
Tests wallet's overall mass estimation including UTXO entries:
- Validates wallet's mass estimation logic with full UTXO context

**Priority:** HIGH - Critical for fee estimation

**Status:** READY - Tests wallet-specific estimation logic

### 3. Utility Tests (`utils/format_kas_test.go`)

#### TestKasToSompi
Tests conversion from KAS string to Sompi (uint64):
- Valid cases:
  - "0" -> 0
  - "1" -> 100000000
  - "33184.1489732" -> 3318414897320
  - "21.35808032" -> 2135808032
  - "184467440737.09551615" -> 18446744073709551615 (max uint64)
- Invalid cases:
  - "184467440737.09551616" (exceeds max uint64)
  - "-1" (negative)
  - "a" (non-numeric)
  - "" (empty string)

**Priority:** MEDIUM - Important utility function

#### TestValidateAmountFormat
Tests validation of KAS amount string format:
- Valid formats: "0", "1", "1.0", "0.1", "0.12345678", etc.
- Max 12 digits left of decimal, 8 digits right
- Invalid formats:
  - Leading zeros: "012", "00.1"
  - Too many decimals: "0.123456789"
  - No integer part: ".1"
  - Extra characters: "0a"
  - Non-numeric: "kaspa"

**Priority:** MEDIUM - Input validation

### 4. ~~BIP32 Tests~~ (EXCLUDED - Tested in kaspa-bip32 dependency)

BIP32 key derivation is handled by the `kaspa-bip32` crate from rusty-kaspa, which has its own comprehensive test suite. We do not need to duplicate these tests.

### 5. ~~Base58 Tests~~ (EXCLUDED - Tested in kaspa-bip32 dependency)

Base58 encoding/decoding is tested in the kaspa-bip32 dependency.

## Rust-Specific Tests Needed

### 1. Encryption/Decryption Tests (`common/src/encrypted_mnemonic.rs`)

#### TestEncryptedMnemonic
- Test encryption and decryption of mnemonics
- Test password correctness validation
- Test that wrong password fails decryption
- Test that encrypted data is different each time (due to random nonce)
- Test various mnemonic lengths (12, 18, 24 words)

**Priority:** HIGH - Security-critical functionality

### 2. Keys Serialization Tests (`common/src/keys.rs`)

#### TestKeysSaveLoad
- Test saving Keys to JSON file
- Test loading Keys from JSON file
- Test that loaded Keys match original
- Test atomic indices are preserved
- Test prefix handling

**Priority:** MEDIUM - Data persistence

### 3. Transaction Encoding Tests (`common/src/transactions_encoding.rs`)

#### TestTransactionEncodingDecoding
- Test encoding WalletSignableTransaction to bytes
- Test decoding bytes back to WalletSignableTransaction
- Test roundtrip preserves all data
- Test with various transaction types (signed, unsigned, multisig)

**Priority:** HIGH - Critical for transaction handling

### 4. Transaction Generator Tests (`daemon/src/transaction_generator.rs`)

#### TestUTXOSelection
- Test UTXO selection for exact amount
- Test UTXO selection with change
- Test UTXO selection for send-all
- Test insufficient funds error
- Test from_addresses filtering
- Test preselected UTXOs

**Priority:** HIGH - Core transaction building

#### TestFeeEstimation
- Test fee estimation for various transaction sizes
- Test fee rate limits (min and max)
- Test max_fee policy
- Test exact_fee_rate policy
- Test max_fee_rate policy

**Priority:** HIGH - Fee calculation accuracy

#### TestTransactionSplitting
- Test automatic transaction splitting when mass exceeds limit
- Test merge transaction creation
- Test split count calculation
- Test input distribution across splits

**Priority:** HIGH - Large transaction handling

#### TestChangeAddress
- Test change address generation
- Test change address reuse when requested
- Test change amount calculation
- Test MIN_CHANGE_TARGET threshold

**Priority:** MEDIUM - Change handling

### 5. Model Conversion Tests (`common/src/model.rs`)

#### TestModelConversions
- Test WalletOutpoint conversions (RPC, Proto, Consensus)
- Test WalletUtxoEntry conversions
- Test WalletUtxo conversions
- Test all From/Into implementations preserve data

**Priority:** MEDIUM - Data integrity

### 6. Error Handling Tests

#### TestWalletErrors
- Test various error types are properly propagated
- Test user input errors vs internal errors
- Test error messages are descriptive

**Priority:** LOW - Error handling validation

## Test Organization

Tests will be **in-file** using `#[cfg(test)]` modules:

```rust
// Example: common/src/encrypted_mnemonic.rs
pub struct EncryptedMnemonic { /* ... */ }

impl EncryptedMnemonic {
    // Implementation code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt() {
        // Test code here
    }

    #[test]
    fn test_wrong_password_fails() {
        // Test code here
    }
}
```

**Benefits of in-file tests:**
- Tests are close to the code they test
- Easy to find relevant tests
- Encourages testing while writing code
- Tests can access private functions and types

**Test Organization:**
- Each `.rs` file contains its own `#[cfg(test)] mod tests` at the bottom
- Use `#[cfg(test)]` for test-only helper functions and utilities
- Use `#[tokio::test]` for async tests
- Mock external dependencies inline in test modules

## Test Utilities Needed

### 1. Mock RPC Client
Create a mock implementation of KaspaRpcClient for unit tests:
- Predictable responses for `get_block_dag_info`
- Predictable responses for `get_fee_estimate`
- No actual network calls

### 3. Test Data Generators
- Generate random but valid transactions
- Generate test UTXOs with various amounts
- Generate test addresses with known derivation paths

## Implementation Priority

### Phase 1: Core Wallet Logic (HIGH priority)
1. **Encrypted mnemonic tests** - Security-critical wallet functionality
2. **Transaction encoding/decoding tests** - Wallet transaction serialization
3. **Mass estimation tests** - Wallet fee estimation logic
4. **UTXO selection tests** - Core transaction building logic
5. **Fee estimation tests** - Wallet fee calculation logic

### Phase 2: Transaction Features (HIGH priority)
6. **Transaction splitting tests** - Wallet logic for handling large transactions
7. **Transaction generator tests** - Integration of transaction building components
8. **Change address tests** - Wallet change handling logic

### Phase 3: Utilities and Supporting Features (MEDIUM priority)
9. **KAS to Sompi conversion tests** - If implemented in wallet (check if this exists)
10. **Amount format validation tests** - If implemented in wallet (check if this exists)
11. **Model conversion tests** - Wallet-specific type conversions
12. **Keys serialization tests** - Wallet data persistence

### Phase 4: Additional Coverage (LOW priority)
13. **Error handling tests** - Wallet error propagation and messages

## Testing Framework

Use Rust's built-in testing framework (`cargo test`) with:
- `#[test]` for unit tests
- `#[cfg(test)]` modules for test-only code
- `proptest` or `quickcheck` for property-based testing (optional)
- `tokio::test` for async tests

## Notes on Porting

1. **Focus on Wallet Logic**: Only test code that the wallet implements, not functionality delegated to dependencies (kaspa-bip32, kaspa-txscript, kaspa-wallet-core, etc.)

2. **Mock External Dependencies**: Use mocks for RPC client and other external dependencies to keep tests fast and deterministic.

3. **Transaction Serialization**: The Rust version currently uses Borsh (with a TODO to move to protobuf). Tests should work with current serialization but be adaptable when protobuf is implemented.

4. **Mass Calculation**: The wallet uses `MassCalculator` from kaspa-wallet-core for actual calculations, but implements estimation logic. Test the estimation logic, not the calculator itself.

5. **Signature Schemes**: Where the wallet has logic that differs based on signature scheme (Schnorr vs ECDSA), tests should cover both.

## Success Criteria

Tests are considered successfully implemented when:
1. All HIGH priority wallet-specific tests are implemented and passing
2. Test coverage is at least 70% for wallet logic modules (excluding dependency code)
3. Tests run in CI/CD pipeline
4. Tests are fast (unit tests complete in < 1 second per test)
5. All wallet logic is tested in isolation with mocked dependencies

## Deferred Items

The following are deferred pending further discussion:
- **Integration tests requiring consensus** (TestMultisig, TestP2PK, TestMaxSompi)
- **Utility function tests** - Need to verify if KAS/Sompi conversion exists in this codebase or is handled elsewhere
