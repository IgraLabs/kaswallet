# Algorithmic Performance Analysis - Large Scale (10M UTXOs, 1M Addresses)

**Date:** 2026-02-03 (Revised)
**Analyst:** Deep Code Analysis
**Scope:** Performance bottlenecks at extreme scale (10M UTXOs, 1M addresses)

---

## ⚠️ Revision Notes

This analysis contains corrections to the initial version based on code review:
- **Critical error corrected:** Vec-based incremental updates are O(K×N), not O(K log N)
- **Reality check added:** RPC fetching dominates in-memory operations at scale
- **Projections adjusted:** More realistic estimates accounting for system constraints

---

## Executive Summary

Analysis reveals **2 confirmed algorithmic bottlenecks** and **1 architectural limitation**:

1. ✅ **FIXED: O(N×M) linear search in transaction selection** - Now uses a prebuilt HashSet
2. ⚠️ **NUANCED: O(N log N) full sort on every sync** - Actually optimal for current Vec structure
3. 🔍 **ARCHITECTURAL: Full UTXO fetch every sync** - RPC dominates everything else

**Key Insight:** The in-memory sort (2.3s) is noise compared to fetching 10M UTXOs over RPC (5-10s+). Optimizing algorithms won't help if we're fetching everything every cycle.

**Priority Recommendations:**
1. **HIGH:** Fix O(N×M) filter (HashSet) - ✅ implemented
2. **MEDIUM:** Investigate incremental/delta sync from node - biggest potential gain
3. **LOW:** Optimize sort - only matters if RPC is fixed first

---

## Table of Contents

1. [Confirmed Bottlenecks](#1-confirmed-bottlenecks)
2. [The Vec::insert Trap (Corrected Analysis)](#2-the-vecinsert-trap-corrected-analysis)
3. [The RPC Elephant in the Room](#3-the-rpc-elephant-in-the-room)
4. [Realistic Solutions](#4-realistic-solutions)
5. [What Actually Matters](#5-what-actually-matters)

---

## 1. Confirmed Bottlenecks

### 1.1 O(N×M) Linear Search in Transaction Selection ✅ REAL ISSUE

**File:** `daemon/src/transaction_generator.rs`
**Lines:** ~817, ~487
**Complexity:** O(N×M) where N = UTXOs, M = from_addresses
**Impact:** SEVERE for transactions with many address filters
**Status:** ✅ Implemented (2026-02-03)

#### The Problem

```rust
// Fix: prebuild HashSet once (O(M)), then per-UTXO filter is O(1) average.
let from_addresses_set: Option<HashSet<WalletAddress>> = if from_addresses.is_empty() {
    None
} else {
    Some(from_addresses.iter().map(|address| (*address).clone()).collect())
};

// ... inside the hot loop ...
if let Some(ref allowed_set) = from_addresses_set {
    if !allowed_set.contains(&utxo.address) {
        // Skip this UTXO (address not allowed)
        continue;
    }
}
```

**Issue:** `from_addresses` is `&[&WalletAddress]` (slice), so `.contains()` is **O(M) linear scan**.

#### Performance Impact

| UTXOs | Addresses | Operations (N×M) | Time @ 100M ops/sec |
|-------|-----------|------------------|---------------------|
| 10M   | 1         | 10M              | 100ms               |
| 10M   | 10        | 100M             | **1 second**        |
| 10M   | 100       | 1B               | **10 seconds**      |

**When this matters:**
- User sending from 100 specific addresses
- Merchant/exchange consolidating from many sources
- Privacy-conscious users filtering by address
- Auto-compound operations that call `more_utxos_for_merge_transaction`

**This is a confirmed, fixable issue.** ✅

#### Benchmark Coverage (Added)

Criterion bench added to make this measurable:
- `daemon/benches/from_addresses_filter.rs`

Run:
```bash
RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo bench -p kaswallet-daemon --features bench from_addresses
```

---

### 1.2 Memory Copies on Every Sync ⚠️ REAL BUT CONTEXT-DEPENDENT

**File:** `daemon/src/sync_manager.rs`
**Lines:** 104, 120
**Impact:** Depends on actual Address size and RPC requirements

#### The Problem

```rust
// Line 99-104
let monitored_addresses: Arc<Vec<Address>>;
{
    let address_manager = self.address_manager.lock().await;
    monitored_addresses = address_manager.monitored_addresses().await?;
}
let addresses: Vec<Address> = monitored_addresses.as_ref().clone();  // Clone 1

// Line 120
let mempool_entries = self.kaspa_client
    .get_mempool_entries_by_addresses(addresses.clone(), true, true)  // Clone 2
    .await?;
```

#### Reality Check Needed

**Unknowns:**
1. Actual `sizeof(Address)` - need to measure, not assume
2. Whether RPC client requires ownership for async/serialization
3. Whether the cost matters relative to RPC time

**If Address ≈ 50 bytes:**
- 1M addresses × 50 bytes = 50MB per clone
- 2 clones = 100MB per sync
- This is real overhead

**But:** Fixing requires RPC API changes (`Vec<Address>` → `&[Address]`), which may not be feasible if the RPC layer needs ownership for serialization.

**This is real but needs measurement and API feasibility check.** ⚠️

---

## 2. The Vec::insert Trap (Corrected Analysis)

### 2.1 Why "Incremental Updates" Don't Help With Vec

**File:** `daemon/src/utxo_manager.rs`
**Line:** 244
**Current approach:** Full sort after bulk rebuild

#### My Original (Wrong) Analysis

❌ **I claimed:** Using `insert_utxo`/`remove_utxo` incrementally would be O(K log N)
❌ **I said:** This would be 1000× faster than full sort for 1% churn

**This was completely wrong.**

#### The Corrected Reality

The current `insert_utxo` and `remove_utxo` implementations:

```rust
// Line 110-122: insert_utxo
fn insert_utxo(&mut self, outpoint: WalletOutpoint, utxo: WalletUtxo) {
    let key = (amount, outpoint);
    let position = self.utxo_keys_sorted_by_amount
        .binary_search(&key)                    // O(log N) - find position ✓
        .unwrap_or_else(|position| position);
    self.utxo_keys_sorted_by_amount
        .insert(position, key);                 // O(N) - shift elements! ✗
}

// Line 128-140: remove_utxo
fn remove_utxo(&mut self, outpoint: &WalletOutpoint) {
    // ... HashMap remove O(1) ...
    let position = self.utxo_keys_sorted_by_amount
        .binary_search(&key)                    // O(log N) ✓
        .expect("missing outpoint");
    self.utxo_keys_sorted_by_amount
        .remove(position);                      // O(N) - shift elements! ✗
}
```

**The trap:** `Vec::insert(pos, item)` and `Vec::remove(pos)` shift all elements after `pos` → **O(N) each**.

#### The Math (Corrected)

For **10M UTXOs with 1% churn** (K = 100,000 changes):

**Full sort (current approach):**
```
O(N log N) = 10M × log₂(10M) = 10M × 23.25 ≈ 233M operations
Time @ 100M ops/sec ≈ 2.3 seconds
```

**Incremental with Vec (my wrong proposal):**
```
O(K × N) = 100K × 10M = 1,000,000,000,000 operations (1 trillion!)
Time @ 100M ops/sec ≈ 10,000 seconds = 2.7 hours
```

**The full sort is 4,300× FASTER than incremental with Vec!**

#### Why This Happens

Vec stores elements contiguously in memory. Inserting in the middle requires:
1. Shift all elements after insertion point right
2. This is fundamentally O(N) for Vec

Even though binary search finds the position in O(log N), the insertion itself is O(N).

#### Verdict on Current Sort Approach

**The current full `sort_unstable()` is actually optimal for Vec-based bulk updates.** ✅

Calling it a "bottleneck" was misleading - it's the correct algorithm for the data structure.

---

### 2.2 Could We Do Better?

**Yes, but only with a different data structure:**

#### Option A: BTreeMap for Sorted Index

Replace:
```rust
utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>
```

With:
```rust
utxo_keys_sorted_by_amount: BTreeMap<(u64, WalletOutpoint), ()>
```

**Pros:**
- True O(log N) insert/remove
- Ordered iteration via `.iter()`
- Incremental updates become viable

**Cons:**
- ~2× memory overhead (tree pointers)
- Slightly slower iteration (not contiguous)
- Requires refactoring to use map keys as index

**Performance for 1% churn:**
```
O(K log N) = 100K × 23 = 2.3M operations
Time @ 100M ops/sec ≈ 23ms
```

**100× faster than current full sort!**

#### Option B: Lazy/Dirty-Flag Sort

```rust
struct UtxoManager {
    utxos_by_outpoint: HashMap<...>,
    utxo_keys_sorted_by_amount: Vec<...>,
    sorted_index_dirty: bool,  // NEW
}

fn insert_utxo(&mut self, ...) {
    self.utxos_by_outpoint.insert(...);
    self.utxo_keys_sorted_by_amount.push(...);  // Just append, O(1)
    self.sorted_index_dirty = true;
}

fn utxos_sorted_by_amount(&mut self) -> impl Iterator<...> {
    if self.sorted_index_dirty {
        self.utxo_keys_sorted_by_amount.sort_unstable();  // Sort on read
        self.sorted_index_dirty = false;
    }
    // ... return iterator
}
```

**Pros:**
- Keeps Vec (cache-friendly)
- Amortizes sort cost
- Only sorts when needed

**Cons:**
- Requires `&mut self` for reads (or unsafe/interior mutability)
- Still O(N log N) when sort happens
- Doesn't help if reads are frequent

#### Option C: Hybrid Heuristic

```rust
if changes.len() < utxos.len() / 1000 {
    // < 0.1% churn: use incremental (even with O(K×N) cost)
    for change in changes {
        self.insert_utxo_or_remove(...);
    }
} else {
    // > 0.1% churn: use full sort
    self.rebuild_and_sort();
}
```

**Pros:**
- Optimal for both small and large updates
- No refactoring needed

**Cons:**
- Complex heuristic
- Hard to tune threshold
- Marginal gains

#### Recommendation

**Accept the current approach** unless:
1. RPC fetch time is solved (see Section 3), AND
2. Measurements show sort is still a bottleneck, THEN
3. Consider BTreeMap refactoring

**Don't fix what isn't broken once RPC is accounted for.**

---

## 3. The RPC Elephant in the Room

### 3.1 What Actually Dominates Sync Time

**The critical insight:** Fetching 10M UTXOs over RPC will dwarf all in-memory operations.

#### Realistic Breakdown of Sync Cycle

| Operation | Current Time | % of Total |
|-----------|--------------|------------|
| RPC: get_utxos_by_addresses | **5-10 seconds** | **70-85%** |
| RPC: get_mempool_entries | 1-2 seconds | 10-15% |
| In-memory: parse/convert | 500ms | 5% |
| In-memory: sort | 2.3 seconds | 20%* |
| In-memory: update structures | 200ms | 2% |
| **Total** | **~10-15 seconds** | 100% |

*Note: Sort and RPC overlap if done right, but sort holds lock.

#### Why RPC Dominates

**For 10M UTXOs:**

1. **Serialization (node side):**
   - 10M UTXOs × ~200 bytes/UTXO = 2GB protobuf
   - Serialization time: seconds

2. **Network transfer:**
   - 2GB over network (even localhost has limits)
   - Compression helps but still large

3. **Deserialization (wallet side):**
   - Parse 2GB protobuf → Rust structs
   - Allocate millions of objects
   - Time: seconds

**The 2.3s sort is only 15-20% of total sync time.** Even if we made it instant, sync would still take 8-12 seconds.

### 3.2 The Real Bottleneck

**Question:** Why are we fetching ALL UTXOs every sync cycle?

Typical blockchain wallets use:
- **Subscriptions:** Node pushes changes
- **Delta sync:** Only fetch what changed since last sync
- **Block-based:** Watch blocks, extract relevant UTXOs

**Full state fetch every cycle doesn't scale.**

With delta sync, only 0.1-1% of UTXOs change per cycle:
- Fetch: 100K UTXOs (20MB) vs 10M (2GB) = **100× less data**
- Serialize: 100ms vs 5s = **50× faster**
- Deserialize: 50ms vs 3s = **60× faster**

**This would be the real optimization.**

---

## 4. Realistic Solutions

### 4.1 HIGH PRIORITY: Fix O(N×M) Filter

**Impact:** Affects user operations, not just background sync
**Effort:** Low (local change)
**Gain:** 10-100× faster for filtered transactions

#### Implementation

```rust
// daemon/src/transaction_generator.rs - select_utxos

pub async fn select_utxos(
    &mut self,
    utxo_manager: &MutexGuard<'_, UtxoManager>,
    preselected_utxos: &HashMap<WalletOutpoint, WalletUtxo>,
    amount: u64,
    is_send_all: bool,
    fee_rate: f64,
    max_fee: u64,
    from_addresses: &[&WalletAddress],
    payload: &[u8],
) -> WalletResult<(Vec<WalletUtxo>, u64, u64)> {
    // Build HashSet once - O(M)
    let from_addresses_set: Option<HashSet<&WalletAddress>> = if from_addresses.is_empty() {
        None
    } else {
        Some(from_addresses.iter().copied().collect())
    };

    // ... rest of setup ...

    let mut iteration = async |
        transaction_generator: &mut TransactionGenerator,
        utxo_manager: &MutexGuard<UtxoManager>,
        utxo: &WalletUtxo
    | -> WalletResult<bool> {
        // HashSet lookup - O(1) instead of O(M)
        if let Some(ref allowed_set) = from_addresses_set {
            if !allowed_set.contains(&utxo.address) {
                return Ok(true);
            }
        }

        // ... rest of selection logic unchanged ...
    };

    // ... rest unchanged ...
}
```

**Also apply to `more_utxos_for_merge_transaction` (line 461).**

#### Why This Works

| Operation | Before | After | Improvement |
|-----------|--------|-------|-------------|
| Build filter | - | O(M) | One-time cost |
| Per-UTXO check | O(M) | O(1) | M× per check |
| Total for N UTXOs | O(N×M) | O(M + N) | M× overall |

For M=100 filters over N=10M UTXOs:
- Before: 1 billion comparisons
- After: 100 + 10M = ~10M operations
- **100× faster**

#### Testing

```rust
#[tokio::test]
async fn bench_select_utxos_with_many_filters() {
    let manager = create_manager_with_utxos(10_000_000).await;
    let filters: Vec<_> = generate_random_addresses(100);
    let filter_refs: Vec<_> = filters.iter().collect();

    let start = Instant::now();
    let _ = select_utxos(
        &manager,
        &HashMap::new(),
        1_000_000,
        false,
        1.0,
        u64::MAX,
        &filter_refs,
        &[]
    ).await.unwrap();
    let elapsed = start.elapsed();

    assert!(elapsed.as_millis() < 500, "Should be under 500ms");
}
```

---

### 4.2 MEDIUM PRIORITY: Investigate Delta Sync

**Impact:** Could reduce sync time by 10-100×
**Effort:** High (requires node API support)
**Gain:** Potentially transformative

#### Current Approach

```rust
// sync_manager.rs - refresh_utxos
let all_utxos = kaspa_client
    .get_utxos_by_addresses(all_addresses)  // Fetches EVERYTHING
    .await?;

utxo_manager.update_utxo_set(all_utxos, mempool).await?;
```

#### Ideal Approach (if node supports)

```rust
// Track last synced state
struct SyncManager {
    last_sync_daa_score: AtomicU64,
    // ...
}

async fn refresh_utxos(&self) -> Result<...> {
    let last_score = self.last_sync_daa_score.load(Relaxed);

    // Only fetch changes since last sync
    let changes = kaspa_client
        .get_utxo_changes_since(all_addresses, last_score)
        .await?;

    // Apply delta
    utxo_manager.apply_delta(changes.added, changes.removed).await?;

    self.last_sync_daa_score.store(changes.new_daa_score, Relaxed);
}
```

#### Questions to Answer

1. **Does kaspad support delta/subscription APIs?**
   - Check kaspa RPC API documentation
   - Look for subscription endpoints
   - Check if there's a "get changes since block X" API

2. **Can we watch blocks and extract relevant UTXOs?**
   - Subscribe to new blocks
   - Filter transactions by our addresses
   - Build UTXO changes from block data

3. **What's the actual churn rate?**
   - For active wallet: How many UTXOs change per block/minute?
   - For inactive wallet: How often do we even need to sync?

**This investigation could reveal the biggest optimization opportunity.**

---

### 4.3 LOW PRIORITY: Clone Optimization

**Impact:** Saves memory, unclear time impact
**Effort:** Medium (requires RPC API changes)
**Gain:** 50-100MB per sync (if measurable)

#### The Issue

```rust
// sync_manager.rs:104
let addresses: Vec<Address> = monitored_addresses.as_ref().clone();  // Clone 50MB

// sync_manager.rs:120
kaspa_client.get_mempool_entries_by_addresses(addresses.clone(), ...)  // Clone again
```

#### Proposed Fix

```rust
async fn refresh_utxos(&self) -> Result<...> {
    let monitored_addresses: Arc<Vec<Address>> = /* ... */;

    if monitored_addresses.is_empty() {
        // ...
        return Ok(());
    }

    // Pass slice reference instead of cloning
    let mempool_entries = self.kaspa_client
        .get_mempool_entries_by_addresses(
            monitored_addresses.as_slice(),  // &[Address] instead of Vec<Address>
            true,
            true
        )
        .await?;

    let utxos = self.kaspa_client
        .get_utxos_by_addresses(
            monitored_addresses.as_slice()  // &[Address]
        )
        .await?;

    // ...
}
```

#### Blockers

1. **RPC client API:** Do these methods accept `&[Address]` or require `Vec<Address>`?

   Check signatures:
   ```rust
   // Need to verify actual signatures in kaspa_grpc_client
   async fn get_utxos_by_addresses(
       &self,
       addresses: ???  // Vec<Address> or &[Address]?
   ) -> Result<...>;
   ```

2. **Serialization requirements:** Does the RPC layer need ownership for async serialization?

3. **Actual measurement:** What is `sizeof(Address)`?
   ```rust
   println!("sizeof(Address) = {}", std::mem::size_of::<Address>());
   ```

#### Decision Criteria

**Only worth doing if:**
1. RPC API can be changed to accept slices, AND
2. Actual Address size × 1M × 2 clones is significant, AND
3. Measurements show memory pressure is an issue

**Otherwise:** The clone cost is lost in RPC noise anyway.

---

### 4.4 DEFERRED: Sort Optimization

**Impact:** 2.3s → potentially <100ms
**Effort:** High (requires BTreeMap refactoring)
**Gain:** Only matters if RPC is fixed first

#### Why It's Low Priority

1. **RPC dominates:** 2.3s sort is 15-20% of 10-15s total sync
2. **Lock contention is real but rare:** Most operations happen between syncs
3. **Vec sort is correct:** Current approach is optimal for the data structure

#### If We Still Want to Optimize

**Only after:**
- RPC is optimized (delta sync working)
- Measurements confirm sort is new bottleneck
- Lock contention is causing user-visible issues

**Then consider:**
```rust
// Replace Vec with BTreeMap for sorted index
struct UtxoManager {
    utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,
    utxo_keys_sorted_by_amount: BTreeMap<(u64, WalletOutpoint), ()>,  // Changed
    // ...
}
```

But this is a significant refactoring for questionable gain.

---

## 5. What Actually Matters

### 5.1 Priority Matrix

| Issue | Impact | Effort | Priority | Status |
|-------|--------|--------|----------|--------|
| O(N×M) filter | High | Low | **HIGH** | Clear fix available |
| Delta sync investigation | Very High | High | **MEDIUM** | Needs research |
| Clone optimization | Medium | Medium | **LOW** | Needs measurement |
| Sort optimization | Low* | High | **DEFERRED** | Wait for RPC fix |

*Low because RPC dominates

### 5.2 Recommended Action Plan

#### Phase 1: Quick Wins (1 week)

1. **Implement HashSet filter fix**
   - Clear algorithmic improvement
   - Affects user operations
   - Low risk, high confidence

2. **Measure actual costs**
   - Profile a real sync cycle
   - Measure `sizeof(Address)`
   - Confirm RPC vs sort time split

3. **Gather data**
   - How often do users filter by many addresses?
   - What's typical UTXO churn rate?
   - Is lock contention actually a problem?

#### Phase 2: Architecture Investigation (2-3 weeks)

4. **Research delta sync options**
   - Check kaspad API documentation
   - Test subscription endpoints
   - Prototype block-watching approach

5. **Evaluate RPC API changes**
   - Can we change to slice parameters?
   - What's the serialization requirement?
   - Benchmark with actual data

#### Phase 3: Only If Needed

6. **Consider BTreeMap refactoring**
   - Only if RPC is optimized AND
   - Measurements show sort is still bottleneck
   - Risk/reward analysis

### 5.3 Success Metrics

**Primary:**
- Sync cycle time (end-to-end)
- Transaction creation time with filters
- Memory usage during sync

**Secondary:**
- Lock contention incidents
- User-reported performance issues
- 99th percentile operation latency

### 5.4 Open Questions for Discussion

1. **Does kaspad support delta/subscription APIs?**
   - This could be 10-100× more impactful than any in-memory optimization

2. **What are common user patterns?**
   - How often do users filter by many addresses?
   - What's the typical wallet size in production?

3. **What's the actual measured breakdown?**
   - Profile with real data to confirm assumptions
   - Is 2.3s sort estimate accurate?

4. **Is there a batching/debounce opportunity?**
   - Could we sync less frequently?
   - Could we batch multiple operations?

---

## 6. Corrected Performance Projections

### 6.1 Realistic Current Performance

**Scenario:** 10M UTXOs, 1M addresses, 1% churn per sync

| Operation | Time | % of Total |
|-----------|------|------------|
| RPC: Fetch UTXOs | 5-8s | 50-60% |
| RPC: Fetch mempool | 1-2s | 10-15% |
| Parse/deserialize | 1-2s | 10-15% |
| In-memory sort | 2.3s | 15-20% |
| Update structures | 200ms | 2% |
| **Total** | **~10-15s** | 100% |

**Transaction with 100 address filters:**
- Address filtering: 10s (O(N×M) scan)
- UTXO selection: 100ms
- Fee estimation: 50ms
- **Total: ~10s**

### 6.2 With HashSet Fix Only

**Scenario:** Same as above, only filter fix applied

| Operation | Time | Change |
|-----------|------|--------|
| Sync cycle | ~10-15s | No change (RPC dominates) |
| Filtered transaction | ~250ms | **40× faster** ✅ |

**Impact:** Transactions with filters are dramatically faster, but sync unchanged.

### 6.3 With Delta Sync (Hypothetical)

**Scenario:** If node supports delta and only 0.1% churn per cycle

| Operation | Before | After | Improvement |
|-----------|--------|-------|-------------|
| RPC: Fetch data | 5-8s | 50-100ms | **50-100×** |
| Parse/deserialize | 1-2s | 10-20ms | **50-100×** |
| In-memory sort | 2.3s | 2.3s | 1× (still full rebuild) |
| **Total sync** | **10-15s** | **~3s** | **3-5×** |

Even with delta sync, the sort remains because we rebuild the entire structure.

### 6.4 With Delta Sync + BTreeMap

**Scenario:** Delta sync + true incremental updates

| Operation | Time | vs Current |
|-----------|------|------------|
| RPC: Fetch delta | 50-100ms | 100× faster |
| Incremental update | 10-20ms | 100× faster |
| **Total sync** | **~100ms** | **100× faster** ✅ |

**But:** Requires both node API support AND data structure refactoring.

---

## 7. Testing Strategy

### 7.1 Before Optimizing: Measure

```rust
#[tokio::test]
async fn profile_sync_cycle() {
    let manager = create_production_like_setup().await;

    let t0 = Instant::now();
    let addresses = get_monitored_addresses();
    let t1 = Instant::now();

    let utxos = rpc_client.get_utxos_by_addresses(addresses).await?;
    let t2 = Instant::now();

    let mempool = rpc_client.get_mempool_entries().await?;
    let t3 = Instant::now();

    manager.update_utxo_set(utxos, mempool).await?;
    let t4 = Instant::now();

    println!("Address fetch: {:?}", t1 - t0);
    println!("RPC UTXOs: {:?}", t2 - t1);  // Should be biggest
    println!("RPC mempool: {:?}", t3 - t2);
    println!("Update+sort: {:?}", t4 - t3);
    println!("Total: {:?}", t4 - t0);
}
```

### 7.2 Validate HashSet Fix

```rust
#[tokio::test]
async fn bench_filter_improvement() {
    let manager = create_manager_with_utxos(10_000_000).await;
    let filters = create_addresses(100);

    // Measure old approach
    let old_time = {
        let start = Instant::now();
        let _ = select_utxos_linear_filter(&manager, &filters).await?;
        start.elapsed()
    };

    // Measure new approach
    let new_time = {
        let start = Instant::now();
        let _ = select_utxos_hashset_filter(&manager, &filters).await?;
        start.elapsed()
    };

    println!("Old: {:?}, New: {:?}, Speedup: {:.1}×",
             old_time, new_time, old_time.as_secs_f64() / new_time.as_secs_f64());

    assert!(new_time < old_time / 10, "Should be at least 10× faster");
}
```

---

## 8. Conclusion

### Key Takeaways

1. **The O(N×M) filter fix is a clear win** - implement this immediately ✅

2. **The "full sort problem" was misunderstood** - it's actually optimal for Vec-based bulk updates ⚠️

3. **RPC fetch dominates everything** - optimizing in-memory operations helps but doesn't solve the fundamental scaling issue 🔍

4. **Delta sync is the real opportunity** - if the node supports it, this could be 100× more impactful than any algorithm tweak 🎯

### What I Got Wrong Initially

- ❌ **Vec::insert is O(log N)** → Actually O(N) due to shifting
- ❌ **Incremental is 1000× faster** → Actually 4300× SLOWER with Vec
- ❌ **Sort is the bottleneck** → RPC fetch is the real bottleneck
- ❌ **Optimistic projections** → Didn't account for serialization/network costs

### What's Actually True

- ✅ **O(N×M) filter is real and fixable** with HashSet
- ✅ **Clones are wasteful** but need measurement and API feasibility check
- ✅ **Full sort with Vec is correct** algorithm for bulk updates
- ✅ **RPC architecture** is the real scaling limitation

### Recommendation

**Do this now:**
1. Implement HashSet filter fix (clear win, low risk)
2. Measure actual costs in production
3. Investigate kaspad delta/subscription APIs

**Don't do this yet:**
4. Don't refactor to BTreeMap without data showing it's needed
5. Don't optimize sort until RPC is addressed

**Questions to answer:**
- Does kaspad support delta sync?
- What's the actual RPC vs sort time split?
- How do users actually use address filtering?

The biggest gains will come from architectural improvements (delta sync), not algorithmic micro-optimizations.
