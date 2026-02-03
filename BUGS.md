# Known Bugs & Issues

**Date:** 2026-02-03
**Status:** Mixed (some fixed, some pending)
**Priority:** See individual bug classifications

---

## Table of Contents

1. [Critical Issues](#1-critical-issues)
2. [High Priority Issues](#2-high-priority-issues)
3. [Medium Priority Issues](#3-medium-priority-issues)
4. [Low Priority Issues](#4-low-priority-issues)

---

## 1. Critical Issues

### 1.1 Sync Interval Default Mismatch

**Severity:** 🔴 CRITICAL
**Impact:** If struct Default is used, wallet becomes completely unusable
**Likelihood:** Low (clap usually wins), but catastrophic if triggered
**Status:** ✅ Fixed (2026-02-03)

#### Description

There was a dangerous mismatch between the clap argument default and the struct Default implementation for `sync_interval_millis`:

- **Clap default:** 10,000ms (10 seconds) ✓
- **Struct Default:** 10ms ✗

If any code path uses the struct Default instead of clap's parsed value, the wallet will attempt to sync every 10ms, which is physically impossible with a 2.3+ second sort for large UTXO sets.

**This is now fixed** by using a shared `DEFAULT_SYNC_INTERVAL_MILLIS` const in `daemon/src/args.rs` for both clap and `Args::default()`.

#### Code Locations

**File:** `daemon/src/args.rs`

```rust
// Clap attribute (fixed to use shared const)
const DEFAULT_SYNC_INTERVAL_MILLIS: u64 = 10_000;

#[arg(
    long,
    default_value_t = DEFAULT_SYNC_INTERVAL_MILLIS,
    help = "Sync interval in milliseconds",
    hide = true
)]
pub sync_interval_millis: u64,

// struct Default impl (fixed to match clap)
impl Default for Args {
    fn default() -> Self {
        Self {
            // ... other fields ...
            sync_interval_millis: DEFAULT_SYNC_INTERVAL_MILLIS,
        }
    }
}
```

#### Why This is Dangerous

1. **Normal case (uses clap):** Works fine with 10 second interval
2. **If struct Default is used:**
   - Sync every 10ms
   - Sort takes 2,300ms for 10M UTXOs
   - **Trying to sync 230× faster than possible**
   - Results in:
     - Infinite sync loop
     - Lock contention
     - CPU at 100%
     - Wallet completely unusable

#### When Struct Default Could Be Used

Potential triggering scenarios:
- Unit tests that construct `Args::default()`
- Internal code that doesn't go through clap parsing
- Configuration management that falls back to Default
- Serialization/deserialization edge cases

#### How to Reproduce (Before Fix)

```rust
// This would trigger the bug:
let args = Args::default();  // Uses struct Default (10ms)
let sync_manager = SyncManager::new(..., args.sync_interval_millis);
// sync_manager will try to sync every 10ms → unusable
```

#### Implemented Fix

Use const for shared default:

```rust
// At top of args.rs
const DEFAULT_SYNC_INTERVAL_MILLIS: u64 = 10_000;

#[arg(
    long,
    default_value_t = DEFAULT_SYNC_INTERVAL_MILLIS,
    help = "Sync interval in milliseconds",
    hide = true
)]
pub sync_interval_millis: u64,

impl Default for Args {
    fn default() -> Self {
        Self {
            // ...
            sync_interval_millis: DEFAULT_SYNC_INTERVAL_MILLIS,
        }
    }
}
```

#### Testing (Added)

```rust
#[test]
fn sync_interval_default_matches_clap_default() {
    let args_from_struct = Args::default();
    let args_from_clap = Args::parse_from(&["kaswallet-daemon"]);

    assert_eq!(
        args_from_struct.sync_interval_millis,
        args_from_clap.sync_interval_millis,
        "Struct Default and clap default must match for sync_interval_millis"
    );
}
```

#### Recommendation

**Implement Option 3** (const for shared default) - this prevents future divergence and makes the intended value explicit.

---

### 1.2 Potential Cache Invalidation Race Condition

**Severity:** 🟠 HIGH
**Impact:** Cache could serve stale data, missing newly added addresses
**Likelihood:** Low (requires specific timing), but possible under load
**Status:** ✅ Verified OK (2026-02-03)

#### Description

The `address_set_version` increment happens AFTER releasing the `addresses` mutex, creating a window where another thread can read the version, see it matches, and return stale cache that doesn't include the newly inserted address.

#### Code Location

**File:** `daemon/src/address_manager.rs`

```rust
// Lines 149-157: new_address method
pub async fn new_address(&self) -> WalletResult<(String, WalletAddress)> {
    // ... address generation ...

    let address_string = address.to_string();
    {
        let mut addresses = self.addresses.lock().await;
        addresses.insert(address_string.clone(), wallet_address.clone());
        self.address_set_version.fetch_add(1, Relaxed);  // Line 154: Inside lock ✓
    }  // Lock released here

    Ok((address_string, wallet_address))
}

// BUT in earlier version (from review), it was OUTSIDE the lock:
// {
//     addresses.insert(...);
// }  // Lock released
// self.address_set_version.fetch_add(1, Relaxed);  // Outside lock ✗
```

**Verified:** In `daemon/src/address_manager.rs`, the version increment is inside the relevant lock scopes for:
- `new_address()` (inside `addresses` lock)
- `change_address()` (inside `addresses` lock)
- `update_addresses_and_last_used_indexes()` (inside `addresses` lock, only if any insertion happens)

There is also a bench-only helper (`bump_address_set_version_for_bench`) that increments without locking; that is acceptable because it is used only to force invalidation in benchmarks.

#### Race Condition Scenario (if version bump was outside lock)

1. **Thread A:** Inserts new address into map
2. **Thread A:** Releases lock
3. **Thread B:** Calls `monitored_addresses()`, reads old version
4. **Thread B:** Version matches cache, returns stale data (missing new address)
5. **Thread A:** Increments version (too late)

#### Verification Notes (Completed)

All production mutation paths bump the version while holding the `addresses` lock, so cache readers either:
- see the old version and legitimately return the old cache (before insert is visible), or
- see the new version and rebuild/return the updated cache.

#### Proposed Fix (if needed)

Ensure version increment is always inside the lock:

```rust
pub async fn new_address(&self) -> WalletResult<(String, WalletAddress)> {
    // ... setup ...

    let address_string = address.to_string();
    {
        let mut addresses = self.addresses.lock().await;
        addresses.insert(address_string.clone(), wallet_address.clone());
        // MUST increment version before releasing lock
        self.address_set_version.fetch_add(1, Relaxed);
    }  // Now safe - version updated atomically with insert

    Ok((address_string, wallet_address))
}
```

#### Testing

```rust
#[tokio::test]
async fn test_no_cache_race_condition() {
    use std::sync::Arc;
    use tokio::task::JoinSet;

    let manager = Arc::new(create_test_manager());
    let mut join_set = JoinSet::new();

    // Spawn 100 tasks that simultaneously add addresses and read cache
    for i in 0..50 {
        let mgr = manager.clone();
        join_set.spawn(async move {
            mgr.new_address().await.unwrap()
        });

        let mgr = manager.clone();
        join_set.spawn(async move {
            mgr.monitored_addresses().await.unwrap()
        });
    }

    // Collect results
    while let Some(_) = join_set.join_next().await {}

    // Final check: cache should match actual state
    let final_cache = manager.monitored_addresses().await.unwrap();
    let final_addresses = manager.address_set().await;

    assert_eq!(
        final_cache.len(),
        final_addresses.len(),
        "Cache and address_set must have same count"
    );

    // Verify all addresses in cache exist in address_set
    for cached_addr in final_cache.as_ref() {
        let addr_string = cached_addr.to_string();
        assert!(
            final_addresses.contains_key(&addr_string),
            "Cached address {} not in address_set",
            addr_string
        );
    }
}
```

#### Recommendation

**Verify current code** - Check lines 154, 222, 346 to confirm version increment is always inside lock. If not, fix immediately.

---

## 2. High Priority Issues

### 2.1 Sync Overlap at Scale

**Severity:** 🟠 HIGH
**Impact:** At 10M UTXOs, wallet is permanently syncing with no idle time
**Likelihood:** High at scale (10M UTXOs)

#### Description

With the default 10-second sync interval and realistic timings at 10M UTXOs:

- **RPC fetch:** 5-10 seconds
- **Sort:** 2.3 seconds
- **Total sync:** ~12-15 seconds
- **Interval:** 10 seconds

**The next sync starts before the previous finishes**, leading to:
- Permanent sync state
- Lock contention
- No idle time for user operations
- Potential queue buildup

#### Code Location

**File:** `daemon/src/sync_manager.rs`

```rust
// Lines 87-94: Sync loop
let mut interval = interval(core::time::Duration::from_millis(self.sync_interval_millis));
loop {
    interval.tick().await;  // Every 10 seconds

    {
        self.sync().await?;  // Takes 12-15 seconds at 10M scale!
    }
}
```

#### Why This Happens

```
Time: 0s     10s    20s    30s
      |      |      |      |
Sync1 |======|======|==|
           Sync2    |======|======|==|
                         Sync3    |======|======|==|
```

Each sync takes 12-15s, but interval is 10s → **overlap and no idle time**.

#### Impact at Different Scales

| UTXOs | Sync Time | Interval | Overlap? | Impact |
|-------|-----------|----------|----------|--------|
| 100K  | ~1s       | 10s      | No       | Fine ✓ |
| 1M    | ~3s       | 10s      | No       | Fine ✓ |
| 10M   | ~12-15s   | 10s      | Yes      | Bad ✗ |

#### Proposed Solutions

**Option 1: Dynamic interval based on last sync time**

```rust
async fn sync_loop(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Initial sync
    self.refresh_utxos().await?;
    self.first_sync_done.store(true, Relaxed);

    loop {
        let start = Instant::now();

        self.sync().await?;

        let elapsed = start.elapsed();

        // Wait at least as long as the sync took, minimum 5s, maximum 30s
        let wait_time = elapsed
            .max(Duration::from_secs(5))
            .min(Duration::from_secs(30));

        debug!("Sync took {:?}, waiting {:?} before next", elapsed, wait_time);
        tokio::time::sleep(wait_time).await;
    }
}
```

**Option 2: Skip sync if previous still running**

```rust
pub struct SyncManager {
    // ...
    sync_in_progress: AtomicBool,
}

async fn sync_loop(&self) -> Result<...> {
    let mut interval = interval(Duration::from_millis(self.sync_interval_millis));

    loop {
        interval.tick().await;

        // Skip if previous sync still running
        if self.sync_in_progress.swap(true, Ordering::Acquire) {
            debug!("Sync still in progress, skipping this interval");
            continue;
        }

        let result = self.sync().await;

        self.sync_in_progress.store(false, Ordering::Release);
        result?;
    }
}
```

**Option 3: Increase default interval**

```rust
// args.rs: Change default to 30 or 60 seconds
default_value = "30000",  // 30 seconds
```

**Option 4: Implement delta sync (long-term solution)**

See ALGORITHMIC-PERF-ANALYSIS.md Section 4.2 for details.

#### Recommendation

**Short-term:** Implement Option 2 (skip if busy) + Option 3 (increase default to 30s)

**Long-term:** Investigate delta sync to reduce sync time to <1s

---

## 3. Medium Priority Issues

### 3.1 Typo: "exculde" Should Be "exclude"

**Severity:** 🟡 MEDIUM (typo, functional but wrong)
**Impact:** Code readability, searching/grepping
**Likelihood:** N/A (always present)

#### Code Location

**File:** `daemon/src/utxo_manager.rs`

```rust
// Line 147
let mut exculde: HashSet<WalletOutpoint> = HashSet::new();

// Line 151
exculde.insert(input.previous_outpoint.into());

// Line 170
if exculde.contains(&wallet_outpoint) {

// Line 223
if exculde.contains(&wallet_outpoint) {
```

#### Fix

Global find-replace:
```bash
# Find all occurrences
rg "exculde" --type rust

# Replace (after review)
sed -i 's/exculde/exclude/g' daemon/src/utxo_manager.rs
```

Or manually:
```rust
let mut exclude: HashSet<WalletOutpoint> = HashSet::new();  // Line 147
exclude.insert(...)  // Line 151
if exclude.contains(...)  // Lines 170, 223
```

---

### 3.2 Typo: "addressed" Should Be "addresses"

**Severity:** 🟡 MEDIUM (log message typo)
**Impact:** Log readability
**Likelihood:** N/A (always present)

#### Code Location

**File:** `daemon/src/sync_manager.rs`

```rust
// Line 316
info!(
    "{} addressed of {} of processed ({:.2}%)",  // "addressed" should be "addresses"
    self.max_processed_addresses_for_log.load(Relaxed),
    self.max_used_addresses_for_log.load(Relaxed),
    percent_processed
);
```

#### Fix

```rust
// Line 316: Fix grammar too
info!(
    "{} addresses of {} processed ({:.2}%)",  // "addresses" and "of processed" → "processed"
    self.max_processed_addresses_for_log.load(Relaxed),
    self.max_used_addresses_for_log.load(Relaxed),
    percent_processed
);
```

---

## 4. Low Priority Issues

### 4.1 Unnecessary Address Vector Clones

**Severity:** 🟢 LOW (performance, not correctness)
**Impact:** 50-100MB allocations per sync (if 1M addresses)
**Likelihood:** Always (but may be unavoidable without RPC API changes)

#### Description

The sync code clones the monitored address list twice per cycle when it could potentially pass references.

#### Code Location

**File:** `daemon/src/sync_manager.rs`

```rust
// Line 104
let addresses: Vec<Address> = monitored_addresses.as_ref().clone();  // Clone 1

// Line 120
let mempool_entries = self.kaspa_client
    .get_mempool_entries_by_addresses(addresses.clone(), true, true)  // Clone 2
    .await?;
```

#### Why This Might Be Necessary

The RPC client methods may require owned `Vec<Address>` for:
- Async task ownership
- Serialization
- Internal buffering

#### Investigation Needed

Check RPC client signatures:
```rust
// Check actual signature in kaspa_grpc_client
pub async fn get_mempool_entries_by_addresses(
    &self,
    addresses: Vec<Address>,  // Requires ownership?
    // vs
    addresses: &[Address],    // Can use reference?
    // ...
) -> Result<...>;
```

#### Potential Fix (if RPC API allows)

```rust
// If RPC client can accept slices:
let mempool_entries = self.kaspa_client
    .get_mempool_entries_by_addresses(
        monitored_addresses.as_slice(),  // No clone
        true,
        true
    )
    .await?;

let utxos = self.kaspa_client
    .get_utxos_by_addresses(
        monitored_addresses.as_slice()  // No clone
    )
    .await?;
```

#### Recommendation

**Investigate RPC API requirements first.** Only fix if:
1. RPC can accept slices/references
2. Measurements show significant memory pressure
3. No other breaking changes needed

---

## 5. Summary & Priorities

### Critical (Fix Immediately)

1. ✅ **Sync interval default mismatch** - One line fix, catastrophic if triggered
   - Fix: Change struct Default from 10 to 10_000

### High (Fix in Next Release)

2. ✅ **Verify cache race condition** - Check if version bump is inside lock
   - Action: Code review of lines 154, 222, 346

3. ✅ **Sync overlap at scale** - Implement skip-if-busy logic
   - Fix: Add `sync_in_progress` flag + increase default interval

### Medium (Fix When Convenient)

4. ✅ **Typo: exculde → exclude** - Simple find-replace
5. ✅ **Typo: addressed → addresses** - Fix log message

### Low (Optional)

6. ⚠️ **Address clones** - Only if RPC API supports and measurements justify

---

## 6. Testing Checklist

After fixes are implemented:

```bash
# Run all tests
cargo test --all

# Specifically test sync behavior
cargo test -p daemon sync_manager
cargo test -p daemon utxo_manager

# Test with different sync intervals
SYNC_INTERVAL=10000 cargo test
SYNC_INTERVAL=30000 cargo test

# Load testing at scale (if available)
cargo test --release bench_sync_at_scale
```

---

## 7. Validation Commands

```bash
# Check for typos
rg "exculde" --type rust  # Should return 0 matches after fix
rg "addressed of" --type rust  # Should return 0 matches after fix

# Verify sync interval defaults
rg "sync_interval_millis.*10[^0]" --type rust  # Should find none

# Check version increment locations
rg "address_set_version.*fetch_add" daemon/src/address_manager.rs -A 2 -B 5

# Build and run
cargo build --release
cargo run --release -- --help  # Verify sync-interval-millis shows 10000
```

---

## 8. Related Documents

- **PERF_OPTIMIZATIONS.md** - Performance improvements implemented
- **PERF_OPTIMIZATIONS-REVIEW.md** - Review of performance work
- **ALGORITHMIC-PERF-ANALYSIS.md** - Deep dive on scaling issues

---

## Appendix: Bug Discovery Timeline

| Date | Bug | Discovered By | Severity |
|------|-----|---------------|----------|
| 2026-02-03 | Sync interval mismatch | Code review | Critical |
| 2026-02-03 | Cache race condition | Code review | High |
| 2026-02-03 | Sync overlap | Performance analysis | High |
| 2026-02-02 | Typo: exculde | Code review | Medium |
| 2026-02-02 | Typo: addressed | Code review | Medium |
| 2026-02-03 | Address clones | Performance analysis | Low |
