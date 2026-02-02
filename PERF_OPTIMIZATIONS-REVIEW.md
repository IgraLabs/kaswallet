# Performance Optimizations - Review & Refinements

**Date:** 2026-02-02
**Reviewer:** Code Analysis
**Status:** Implementation Complete - Refinements Recommended

---

## Executive Summary

The performance optimizations are **comprehensively implemented** and well-tested. The core improvements for UTXO scaling and address discovery are solid. This document identifies refinements to improve consistency, fix minor bugs, and ensure robustness.

**Overall Assessment:** ✅ Strong implementation with minor issues to address

---

## Table of Contents

1. [Critical Issues](#1-critical-issues)
2. [Important Optimizations](#2-important-optimizations)
3. [Minor Issues](#3-minor-issues)
4. [Documentation Gaps](#4-documentation-gaps)
5. [Testing Recommendations](#5-testing-recommendations)

---

## 1. Critical Issues

### 1.1 Potential Race Condition in Cache Invalidation

**File:** `daemon/src/address_manager.rs`
**Lines:** 126-133 (new_address) and 309-316 (change_address)
**Severity:** High
**Impact:** Cache could serve stale data, missing newly added addresses

#### Problem

The current code inserts an address into the map and THEN increments the version:

```rust
// Current implementation (new_address, lines 126-133)
{
    self.addresses
        .lock()
        .await
        .insert(address.to_string(), wallet_address.clone());
}
self.address_set_version.fetch_add(1, Relaxed);
```

**Race condition scenario:**
1. Thread A: Inserts new address into `addresses` map (lock released)
2. Thread B: Calls `monitored_addresses()`, reads old version, returns cached data
3. Thread A: Increments version
4. Result: Thread B has stale cache missing the new address

#### Why This Matters

The `monitored_addresses()` method (lines 81-107) checks the version OUTSIDE the lock:

```rust
let current_version = self.address_set_version.load(Relaxed);
{
    let cache = self.monitored_addresses_cache.lock().await;
    if cache.version == current_version {
        return Ok(cache.addresses.clone());  // Returns stale data!
    }
}
```

#### Proposed Fix

**Option 1: Increment version while holding the lock**

```rust
// daemon/src/address_manager.rs - new_address method

pub async fn new_address(&self) -> WalletResult<(String, WalletAddress)> {
    let last_used_external_index_previous_value = self
        .keys_file
        .last_used_external_index
        .fetch_add(1, Relaxed);
    let last_used_external_index = last_used_external_index_previous_value + 1;
    self.keys_file.save()?;

    let wallet_address = WalletAddress::new(
        last_used_external_index,
        self.keys_file.cosigner_index,
        Keychain::External,
    );
    let address = self
        .kaspa_address_from_wallet_address(&wallet_address, true)
        .await?;

    {
        let mut addresses_guard = self.addresses.lock().await;
        addresses_guard.insert(address.to_string(), wallet_address.clone());
        // Increment version BEFORE releasing the lock
        self.address_set_version.fetch_add(1, Relaxed);
    }
    // Version increment happens atomically with the insert from cache reader's perspective

    Ok((address.to_string(), wallet_address))
}
```

**Apply the same fix to `change_address` method (lines 280-318):**

```rust
// daemon/src/address_manager.rs - change_address method

pub async fn change_address(
    &self,
    use_existing_change_address: bool,
    from_addresses: &[&WalletAddress],
) -> WalletResult<(Address, WalletAddress)> {
    let wallet_address = if !from_addresses.is_empty() {
        from_addresses[0].clone()
    } else {
        let internal_index = if use_existing_change_address {
            0
        } else {
            self.keys_file
                .last_used_internal_index
                .fetch_add(1, Relaxed)
                + 1
        };
        self.keys_file.save()?;

        WalletAddress::new(
            internal_index,
            self.keys_file.cosigner_index,
            Keychain::Internal,
        )
    };

    let address = self
        .kaspa_address_from_wallet_address(&wallet_address, true)
        .await?;

    {
        let mut addresses_guard = self.addresses.lock().await;
        addresses_guard.insert(address.to_string(), wallet_address.clone());
        // Increment version BEFORE releasing the lock
        self.address_set_version.fetch_add(1, Relaxed);
    }

    Ok((address, wallet_address))
}
```

#### Why This Fix Works

1. **Atomicity from reader's perspective:** When `monitored_addresses()` reads the version, it will either see:
   - Old version → returns old cache (correct, new address hasn't been inserted yet from its perspective)
   - New version → rebuilds cache (correct, includes new address)

2. **No window for inconsistency:** The version increment happens before the lock is released, so there's no gap where the map is updated but version is stale.

3. **Memory ordering:** `Relaxed` ordering is still safe here because the mutex provides the necessary synchronization barriers.

#### How to Test

```rust
#[tokio::test]
async fn monitored_addresses_cache_no_race_condition() {
    use std::sync::Arc;
    use tokio::task::JoinSet;

    let keys = keys_with_no_pubkeys();
    let manager = Arc::new(AddressManager::new(keys, Prefix::Mainnet));

    let mut join_set = JoinSet::new();

    // Spawn 100 tasks that simultaneously add addresses and read monitored addresses
    for i in 0..50 {
        let mgr = manager.clone();
        join_set.spawn(async move {
            mgr.new_address().await.unwrap();
        });

        let mgr = manager.clone();
        join_set.spawn(async move {
            let monitored = mgr.monitored_addresses().await.unwrap();
            monitored.len()
        });
    }

    // Wait for all tasks
    while let Some(_) = join_set.join_next().await {}

    // Final monitored addresses should include all 50 added addresses
    let final_monitored = manager.monitored_addresses().await.unwrap();
    let address_set = manager.address_set().await;
    assert_eq!(final_monitored.len(), address_set.len());
}
```

---

## 2. Important Optimizations

### 2.1 GetBalance Not Fully Optimized

**File:** `daemon/src/service/get_balance.rs`
**Lines:** 7-64
**Severity:** Medium
**Impact:** Suboptimal performance on GetBalance requests for wallets with many addresses

#### Problem

The `get_balance` method has two optimization opportunities:

1. **No early return for empty wallet** (like `refresh_utxos` has)
2. **Inefficient address lookups** - converts each wallet_address individually instead of pre-building a lookup map (unlike the optimized `get_utxos`)

Current implementation (lines 36-41):
```rust
let address_manager = self.address_manager.lock().await;
for (wallet_address, balances) in &balances_map {
    let address = address_manager
        .kaspa_address_from_wallet_address(wallet_address, true)
        .await
        .to_wallet_result_internal()?;
    // ... use address
}
```

This acquires the lock once but calls `kaspa_address_from_wallet_address()` N times (where N = number of unique addresses with balance). While caching helps, this pattern differs from the optimized `get_utxos`.

#### Proposed Fix

Apply the same optimization pattern used in `get_utxos.rs`:

```rust
// daemon/src/service/get_balance.rs

use crate::address_manager::AddressSet;
use crate::service::kaswallet_service::KasWalletService;
use common::errors::{ResultExt, WalletResult};
use common::model::WalletAddress;
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

        // Early return for empty wallet (like refresh_utxos does)
        let utxos_count: usize;
        {
            let utxo_manager = self.utxo_manager.lock().await;
            utxos_count = utxo_manager.utxos_by_outpoint().len();
            if utxos_count == 0 {
                return Ok(GetBalanceResponse {
                    available: 0,
                    pending: 0,
                    address_balances: vec![],
                });
            }
        }

        // Pre-build wallet_address -> string lookup (like get_utxos does)
        let address_set: AddressSet;
        {
            let address_manager = self.address_manager.lock().await;
            address_set = address_manager.address_set().await;
        }
        let wallet_address_to_string: HashMap<WalletAddress, String> = address_set
            .iter()
            .map(|(address_string, wallet_address)| {
                (wallet_address.clone(), address_string.clone())
            })
            .collect();

        // Calculate balances
        let mut balances_map = HashMap::new();
        {
            let utxo_manager = self.utxo_manager.lock().await;
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

        // Build response using pre-built lookup
        let mut address_balances = vec![];
        let mut total_balances = BalancesEntry::new();

        for (wallet_address, balances) in &balances_map {
            // Fast lookup instead of derivation
            let address_string = wallet_address_to_string
                .get(wallet_address)
                .ok_or_else(|| {
                    common::errors::WalletError::InternalServerError(format!(
                        "wallet address missing from address_set: {:?}",
                        wallet_address
                    ))
                })?;

            if request.include_balance_per_address {
                address_balances.push(AddressBalances {
                    address: address_string.clone(),
                    available: balances.available,
                    pending: balances.pending,
                });
            }
            total_balances.add(balances);
        }

        info!(
            "GetBalance request scanned {} UTXOs over {} addresses",
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
```

#### Why This Fix Works

1. **Early return:** Avoids unnecessary work when wallet is empty (0 UTXOs → 0 balance)
2. **Pre-built lookup:** Single O(N) map inversion instead of N lookups
3. **Consistent pattern:** Matches the optimized `get_utxos` implementation
4. **Cache-friendly:** The `wallet_address_to_string` map is built once per request

#### Performance Impact

- **Before:** For wallet with 1000 addresses, potential 1000 hash lookups + cache checks
- **After:** Single O(N) map build, then O(1) lookups for addresses with balances
- **Empty wallet:** Returns immediately without any UTXO iteration

#### How to Test

```rust
#[tokio::test]
async fn get_balance_empty_wallet_returns_immediately() {
    // Setup service with empty UTXO set
    let service = create_test_service_with_empty_utxos().await;

    let request = GetBalanceRequest {
        include_balance_per_address: true,
    };

    let response = service.get_balance(request).await.unwrap();

    assert_eq!(response.available, 0);
    assert_eq!(response.pending, 0);
    assert_eq!(response.address_balances.len(), 0);
}

#[tokio::test]
async fn get_balance_with_many_addresses_is_fast() {
    use std::time::Instant;

    // Setup service with 1000 addresses, 500 with UTXOs
    let service = create_test_service_with_many_addresses(1000, 500).await;

    let start = Instant::now();
    let response = service.get_balance(GetBalanceRequest {
        include_balance_per_address: true,
    }).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(response.address_balances.len(), 500);
    assert!(elapsed.as_millis() < 100, "GetBalance should be fast even with many addresses");
}
```

---

## 3. Minor Issues

### 3.1 Typo: "exculde" → "exclude"

**File:** `daemon/src/utxo_manager.rs`
**Line:** 147
**Severity:** Low (typo, doesn't affect functionality)

#### Current Code
```rust
let mut exculde: HashSet<WalletOutpoint> = HashSet::new();
```

#### Fix
```rust
let mut exclude: HashSet<WalletOutpoint> = HashSet::new();
```

Also update all references:
- Line 151: `exculde.insert(...)`
- Line 178: `if exculde.contains(...)`
- Line 228: `if exculde.contains(...)`

#### Complete Fix

```rust
// daemon/src/utxo_manager.rs - update_utxo_set method (lines 142-253)

pub async fn update_utxo_set(
    &mut self,
    rpc_utxo_entries: Vec<RpcUtxosByAddressesEntry>,
    rpc_mempool_utxo_entries: Vec<RpcMempoolEntryByAddress>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut exclude: HashSet<WalletOutpoint> = HashSet::new();  // Fixed typo
    for rpc_mempool_entries_by_address in &rpc_mempool_utxo_entries {
        for sending_rpc_mempool_entry in &rpc_mempool_entries_by_address.sending {
            for input in &sending_rpc_mempool_entry.transaction.inputs {
                exclude.insert(input.previous_outpoint.into());  // Fixed reference
            }
        }
    }

    let address_set: AddressSet;
    {
        let address_manager = self.address_manager.lock().await;
        address_set = address_manager.address_set().await;
    }

    let mut address_map = HashMap::with_capacity(address_set.len());
    for (address_string, wallet_address) in &address_set {
        let address = Address::try_from(address_string.as_str()).map_err(|err| {
            format!("invalid address in wallet address_set ({address_string}): {err}")
        })?;
        address_map.insert(address, wallet_address.clone());
    }

    // Rebuild from scratch while reusing allocations where possible.
    self.utxos_by_outpoint.clear();
    self.utxo_keys_sorted_by_amount.clear();
    self.utxos_by_outpoint.reserve(rpc_utxo_entries.len());
    self.utxo_keys_sorted_by_amount.reserve(rpc_utxo_entries.len());

    for rpc_utxo_entry in rpc_utxo_entries {
        let wallet_outpoint: WalletOutpoint = rpc_utxo_entry.outpoint.into();
        if exclude.contains(&wallet_outpoint) {  // Fixed reference
            continue;
        }

        let wallet_utxo_entry: WalletUtxoEntry = rpc_utxo_entry.utxo_entry.into();
        let amount = wallet_utxo_entry.amount;

        let Some(address) = &rpc_utxo_entry.address else {
            continue;
        };
        let wallet_address = address_map
            .get(address)
            .ok_or_else(|| {
                format!(
                    "UTXO address {} not found in wallet address_set",
                    address.to_string()
                )
            })?
            .clone();

        let wallet_utxo =
            WalletUtxo::new(wallet_outpoint.clone(), wallet_utxo_entry, wallet_address);

        self.utxos_by_outpoint
            .insert(wallet_outpoint.clone(), wallet_utxo);
        self.utxo_keys_sorted_by_amount
            .push((amount, wallet_outpoint));
    }

    for rpc_mempool_entry in rpc_mempool_utxo_entries {
        for receiving_rpc_mempool_entry in &rpc_mempool_entry.receiving {
            let transaction = &receiving_rpc_mempool_entry.transaction;
            let Some(transaction_verbose_data) = &transaction.verbose_data else {
                panic!("transaction verbose data missing")
            };
            for (i, output) in transaction.outputs.iter().enumerate() {
                let Some(output_verbose_data) = &output.verbose_data else {
                    panic!("output verbose data missing")
                };
                let address_string = output_verbose_data
                    .script_public_key_address
                    .address_to_string();
                let Some(address) = address_set.get(&address_string) else {
                    // this means this output is not to this wallet
                    continue;
                };

                let wallet_outpoint =
                    WalletOutpoint::new(transaction_verbose_data.transaction_id, i as u32);

                if exclude.contains(&wallet_outpoint) {  // Fixed reference
                    continue;
                }
                let utxo_entry = WalletUtxoEntry::new(
                    output.value,
                    output.script_public_key.clone(),
                    0,
                    false,
                );

                let utxo =
                    WalletUtxo::new(wallet_outpoint.clone(), utxo_entry, address.clone());

                self.utxos_by_outpoint
                    .insert(wallet_outpoint.clone(), utxo);
                self.utxo_keys_sorted_by_amount
                    .push((output.value, wallet_outpoint));
            }
        }
    }

    self.utxo_keys_sorted_by_amount.sort_unstable();

    self.apply_mempool_transactions_after_update().await;
    Ok(())
}
```

---

### 3.2 Typo: "addressed" → "addresses"

**File:** `daemon/src/sync_manager.rs`
**Line:** 316
**Severity:** Low (typo in log message)

#### Current Code
```rust
info!(
    "{} addressed of {} of processed ({:.2}%)",
    self.max_processed_addresses_for_log.load(Relaxed),
    self.max_used_addresses_for_log.load(Relaxed),
    percent_processed
);
```

#### Fix
```rust
info!(
    "{} addresses of {} processed ({:.2}%)",
    self.max_processed_addresses_for_log.load(Relaxed),
    self.max_used_addresses_for_log.load(Relaxed),
    percent_processed
);
```

**Note:** Also improved grammar ("of processed" → clearer phrasing)

---

### 3.3 Redundant Method in WalletUtxo

**File:** `common/src/proto_convert.rs`
**Lines:** 164-177
**Severity:** Low (API inconsistency)

#### Problem

The `WalletUtxo` implementation has both `to_proto(&self)` and `into_proto(self)`:

```rust
impl WalletUtxo {
    pub fn to_proto(&self, is_pending: bool, is_dust: bool) -> ProtoUtxo {
        ProtoUtxo {
            outpoint: Some(self.outpoint.clone().into()),
            utxo_entry: Some(ProtoUtxoEntry::from(&self.utxo_entry)),
            is_pending,
            is_dust,
        }
    }

    pub fn into_proto(self, is_pending: bool, is_dust: bool) -> ProtoUtxo {
        self.to_proto(is_pending, is_dust)  // Just calls to_proto!
    }
}
```

**Issues:**
1. `into_proto` signature suggests it consumes `self` (move semantics)
2. But it actually just calls `to_proto` which borrows
3. This is misleading and provides no performance benefit
4. Violates Rust naming conventions (into_* should consume)

#### Proposed Fix

**Option 1: Remove `into_proto` entirely**

```rust
impl WalletUtxo {
    pub fn to_proto(&self, is_pending: bool, is_dust: bool) -> ProtoUtxo {
        ProtoUtxo {
            outpoint: Some(self.outpoint.clone().into()),
            utxo_entry: Some(ProtoUtxoEntry::from(&self.utxo_entry)),
            is_pending,
            is_dust,
        }
    }

    // Remove into_proto entirely
}
```

Check if any code calls `into_proto`:
```bash
grep -r "into_proto" --include="*.rs" .
```

If found, replace with `to_proto`.

**Option 2: Make `into_proto` actually consume (only if there's a perf benefit)**

If you want true move semantics to avoid cloning:

```rust
impl WalletUtxo {
    pub fn to_proto(&self, is_pending: bool, is_dust: bool) -> ProtoUtxo {
        ProtoUtxo {
            outpoint: Some(self.outpoint.clone().into()),
            utxo_entry: Some(ProtoUtxoEntry::from(&self.utxo_entry)),
            is_pending,
            is_dust,
        }
    }

    pub fn into_proto(self, is_pending: bool, is_dust: bool) -> ProtoUtxo {
        ProtoUtxo {
            outpoint: Some(self.outpoint.into()),  // No clone
            utxo_entry: Some(self.utxo_entry.into()),  // No clone
            is_pending,
            is_dust,
        }
    }
}
```

But this requires `WalletUtxoEntry` to have `Into<ProtoUtxoEntry>` that consumes. Currently only `From<&WalletUtxoEntry>` exists.

#### Recommendation

**Remove `into_proto`** - it provides no benefit and is misleading. The doc explicitly says optimizations avoid cloning, so `to_proto(&self)` is the right API.

---

## 4. Documentation Gaps

### 4.1 Missing Files in Git Status

**Files mentioned in PERF_OPTIMIZATIONS.md but not in `git status`:**
- `common/src/addresses.rs`
- `daemon/src/address_manager.rs`

**Files in `git status` but not mentioned in doc:**
- `client/src/client.rs`

#### Investigation Needed

Run these commands to check:

```bash
# Check if these files have uncommitted changes
git diff common/src/addresses.rs
git diff daemon/src/address_manager.rs

# Check if they were already committed
git log --oneline --follow common/src/addresses.rs | head -5
git log --oneline --follow daemon/src/address_manager.rs | head -5

# Check what changed in client.rs
git diff client/src/client.rs
```

#### Possible Explanations

1. **Already committed:** Files were modified and committed in an earlier commit
2. **Staging issue:** Files need to be staged with `git add`
3. **Documentation error:** Doc needs updating to reflect actual changes

#### Recommended Fix for PERF_OPTIMIZATIONS.md

Update the "Files touched" section to match actual changes:

```markdown
## Files touched

- Updated:
  - `client/src/client.rs` (documentation updates)
  - `common/src/addresses.rs`
  - `common/src/model.rs`
  - `common/src/proto_convert.rs`
  - `daemon/src/address_manager.rs`
  - `daemon/src/service/get_balance.rs`
  - `daemon/src/service/get_utxos.rs`
  - `daemon/src/sync_manager.rs`
  - `daemon/src/transaction_generator.rs`
  - `daemon/src/utxo_manager.rs`
- Added:
  - `PERF_OPTIMIZATIONS.md`
```

And add a note explaining client.rs changes:

```markdown
### Client API (client/src/client.rs)

No functional changes - updated documentation comments to reflect improved performance characteristics.
```

---

## 5. Testing Recommendations

### 5.1 Concurrency Tests

Add tests to verify thread-safety of optimizations:

```rust
// daemon/src/address_manager.rs - add to tests module

#[tokio::test]
async fn concurrent_address_operations_maintain_cache_consistency() {
    use std::sync::Arc;
    use tokio::task::JoinSet;

    let keys = keys_with_single_pubkey();
    let manager = Arc::new(AddressManager::new(keys, Prefix::Mainnet));

    let mut join_set = JoinSet::new();

    // Spawn 50 concurrent operations
    for _ in 0..50 {
        // Mix of address creation and cache reads
        let mgr = manager.clone();
        join_set.spawn(async move {
            mgr.new_address().await.unwrap();
        });

        let mgr = manager.clone();
        join_set.spawn(async move {
            mgr.monitored_addresses().await.unwrap()
        });
    }

    // Wait for all to complete
    let mut results = vec![];
    while let Some(result) = join_set.join_next().await {
        results.push(result.unwrap());
    }

    // Verify final consistency
    let final_monitored = manager.monitored_addresses().await.unwrap();
    let final_address_set = manager.address_set().await;

    // Cache should reflect actual state
    assert_eq!(final_monitored.len(), final_address_set.len());

    // All addresses in cache should be in address_set
    for cached_addr in final_monitored.as_ref() {
        let addr_string = cached_addr.to_string();
        assert!(final_address_set.contains_key(&addr_string),
            "Cached address {} not found in address_set", addr_string);
    }
}
```

### 5.2 Performance Benchmarks

Add benchmarks to validate improvements:

```rust
// daemon/src/service/get_balance.rs - add benchmark

#[cfg(test)]
mod benches {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn benchmark_get_balance_scaling() {
        for num_addresses in [100, 1000, 10000] {
            let service = create_test_service_with_addresses(num_addresses).await;

            let start = Instant::now();
            let _ = service.get_balance(GetBalanceRequest {
                include_balance_per_address: true,
            }).await.unwrap();
            let elapsed = start.elapsed();

            println!(
                "GetBalance with {} addresses: {:?}",
                num_addresses,
                elapsed
            );

            // Should scale linearly, not quadratically
            // With 10k addresses, should complete in < 500ms
            if num_addresses == 10000 {
                assert!(elapsed.as_millis() < 500,
                    "GetBalance too slow: {:?}", elapsed);
            }
        }
    }
}
```

### 5.3 Regression Tests

Ensure optimizations don't break existing functionality:

```rust
// daemon/src/utxo_manager.rs - add regression test

#[tokio::test]
async fn utxo_sorted_iteration_matches_full_sort() {
    let mut manager = make_manager();

    // Insert UTXOs in random order
    let mut utxos = vec![
        make_utxo(50, 1, 0),
        make_utxo(10, 2, 0),
        make_utxo(100, 3, 0),
        make_utxo(50, 4, 0),
        make_utxo(25, 5, 0),
    ];

    for utxo in &utxos {
        manager.insert_utxo(utxo.outpoint.clone(), utxo.clone());
    }

    // Get amounts via optimized iterator
    let amounts_from_iterator: Vec<u64> = manager
        .utxos_sorted_by_amount()
        .map(|u| u.utxo_entry.amount)
        .collect();

    // Get amounts via full sort (reference implementation)
    utxos.sort_by_key(|u| (u.utxo_entry.amount, u.outpoint.clone()));
    let expected_amounts: Vec<u64> = utxos
        .iter()
        .map(|u| u.utxo_entry.amount)
        .collect();

    assert_eq!(amounts_from_iterator, expected_amounts);
}
```

---

## 6. Summary of Action Items

### Critical (Must Fix)
- [ ] **1.1** - Fix race condition in cache invalidation (address_manager.rs)

### Important (Should Fix)
- [ ] **2.1** - Optimize GetBalance method (get_balance.rs)

### Minor (Nice to Have)
- [ ] **3.1** - Fix typo "exculde" → "exclude" (utxo_manager.rs:147)
- [ ] **3.2** - Fix typo "addressed" → "addresses" (sync_manager.rs:316)
- [ ] **3.3** - Remove redundant `into_proto` method (proto_convert.rs:174-176)

### Documentation
- [ ] **4.1** - Reconcile file list in PERF_OPTIMIZATIONS.md with git status
- [ ] **4.1** - Document client.rs changes

### Testing
- [ ] **5.1** - Add concurrency test for address cache
- [ ] **5.2** - Add performance benchmarks
- [ ] **5.3** - Add regression tests

---

## Validation Commands

Run these after implementing fixes:

```bash
# Run all tests
RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo test -q

# Run specific module tests
cargo test -p daemon address_manager
cargo test -p daemon get_balance
cargo test -p daemon utxo_manager

# Check for remaining typos
grep -r "exculde" --include="*.rs" .
grep -r "addressed of" --include="*.rs" .

# Verify no usage of removed into_proto
grep -r "\.into_proto(" --include="*.rs" .

# Run clippy for additional warnings
cargo clippy --all-targets --all-features
```

---

## Conclusion

The performance optimizations are **solid and well-implemented**. The issues identified are refinements rather than fundamental problems:

- **1 critical issue** (race condition) - easy fix with high confidence
- **1 important optimization** (GetBalance) - follows existing pattern
- **3 minor issues** (typos, redundant code) - trivial fixes
- **Documentation gaps** - need reconciliation

**Estimated effort:** 2-4 hours to implement all fixes and tests

**Risk level:** Low - fixes are localized and follow established patterns

The core innovations (incremental scanning, address caching, iterator-based UTXO access) are excellent and will provide significant performance improvements at scale.
