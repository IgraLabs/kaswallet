# Lock Contention Solution: Double Buffering for UTXO Manager

**Date:** 2026-02-03 (Revised after team review)
**Author:** Technical Analysis
**Status:** Proposal for Review

---

## ⚠️ Revision Notes

This document has been updated based on team review to correct:
- **Critical:** Explicitly remove outer Mutex, not just add ArcSwap inside it
- **Critical:** Fix iterator pattern to avoid impossible lifetimes
- **Critical:** Redesign mempool updates to avoid cloning entire state
- **Clarified:** RwLock vs ArcSwap trade-offs
- **Marked:** All memory numbers as placeholders requiring measurement

---

## Executive Summary

**Problem:** At scale (10M UTXOs), readers (GetUtxos, GetBalance, transaction creation) are blocked 23% of the time waiting for sync to complete (2.3s sort / 10s interval).

**Root Cause:** `UtxoManager` is wrapped in `Arc<Mutex<UtxoManager>>` with exclusive access. During sync's 2.3+ second update, all readers block on the Mutex.

**Solution:** Remove the outer Mutex entirely. Use `Arc<UtxoManager>` directly with `ArcSwap` inside for lock-free reads. Build new state without holding locks, then atomically swap.

**Impact:**
- **Availability:** Readers blocked <0.1% of time (vs 23% currently)
- **Memory:** 2× peak during transition (needs measurement to confirm risk)
- **Complexity:** High - requires removing Mutex from all call sites
- **Breaking change:** All code accessing UtxoManager needs updates

---

## Table of Contents

1. [Current Problem Analysis](#1-current-problem-analysis)
2. [Why Readers Block](#2-why-readers-block)
3. [Proposed Solution: Remove Outer Mutex + ArcSwap](#3-proposed-solution-remove-outer-mutex--arcswap)
4. [Detailed Implementation](#4-detailed-implementation)
5. [Mempool Update Strategy](#5-mempool-update-strategy)
6. [Memory & Performance Analysis](#6-memory--performance-analysis)
7. [Migration Path](#7-migration-path)
8. [RwLock Alternative](#8-rwlock-alternative)
9. [Testing Strategy](#9-testing-strategy)
10. [Risks & Mitigations](#10-risks--mitigations)

---

## 1. Current Problem Analysis

### 1.1 The Lock Contention Issue

**Current architecture:**

```rust
// daemon/src/sync_manager.rs:24
pub struct SyncManager {
    utxo_manager: Arc<Mutex<UtxoManager>>,  // ← THE PROBLEM: Outer Mutex
    // ...
}

// daemon/src/utxo_manager.rs:11
pub struct UtxoManager {
    address_manager: Arc<Mutex<AddressManager>>,
    coinbase_maturity: u64,

    // Data lives INSIDE the struct
    utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,
    utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>,
    mempool_transactions: Vec<WalletSignableTransaction>,
}
```

**Every operation requires locking:**

```rust
// Sync (writer)
let mut utxo_manager = self.utxo_manager.lock().await;  // ← Exclusive lock
utxo_manager.update_utxo_set(...).await?;
// Held for 2.3+ seconds

// GetUtxos (reader)
let utxo_manager = self.utxo_manager.lock().await;  // ← Blocks if sync running
for utxo in utxo_manager.utxos_sorted_by_amount() { ... }

// GetBalance (reader)
let utxo_manager = self.utxo_manager.lock().await;  // ← Blocks if sync running
for utxo in utxo_manager.utxos_by_outpoint().values() { ... }
```

**The fundamental issue:** `Mutex` is exclusive. Even though readers don't modify data, they still need the lock, so they block on the writer (sync).

### 1.2 Timeline at 10M UTXOs

```
Time:     0ms        500ms       1000ms      1500ms      2000ms      2500ms
          |-----------|-----------|-----------|-----------|-----------|
Sync:     [clear]    [build]     [build]     [sort]      [sort]      [done]
Mutex:    [==========HELD FOR 2300ms==========================================]

GetUtxos:                        [BLOCKED..................][execute]
GetBalance:                                [BLOCKED............][execute]
SendTx:                                           [BLOCKED......][execute]
```

### 1.3 Why This Happens

**Inside update_utxo_set (daemon/src/utxo_manager.rs:142-247):**

```rust
pub async fn update_utxo_set(
    &mut self,  // ← Requires exclusive lock on UtxoManager
    rpc_utxo_entries: Vec<...>,
    rpc_mempool_utxo_entries: Vec<...>,
) -> Result<...> {
    // DESTROYS OLD DATA
    self.utxos_by_outpoint.clear();           // ← Old data gone
    self.utxo_keys_sorted_by_amount.clear();  // ← Old data gone

    // BUILD NEW DATA (500-1000ms)
    for rpc_utxo_entry in rpc_utxo_entries {
        self.utxos_by_outpoint.insert(...);
        self.utxo_keys_sorted_by_amount.push(...);
    }

    // SORT (2.3 seconds at 10M UTXOs)
    self.utxo_keys_sorted_by_amount.sort_unstable();

    Ok(())
}
```

**During this time, data is in inconsistent state:**
- T=100ms: Empty (just cleared)
- T=1000ms: Partially built, unsorted
- T=2300ms: Complete and sorted

**Readers MUST be blocked** to avoid seeing inconsistent state.

---

## 2. Why Readers Block

### 2.1 The Outer Mutex Problem

**Question:** Why can't readers use the "old" data while sync builds the "new" data?

**Answer 1:** The outer `Mutex<UtxoManager>` means ANY access requires locking, even to call methods on UtxoManager.

**Answer 2:** Even if we added `ArcSwap` inside UtxoManager, readers would still need to lock the Mutex to access it:

```rust
// WRONG APPROACH - doesn't help!
pub struct UtxoManager {
    current_state: ArcSwap<UtxoState>,  // ← Lock-free inside
}

// But still:
utxo_manager: Arc<Mutex<UtxoManager>>  // ← Outer Mutex still blocks!

// Readers still do:
let mgr = self.utxo_manager.lock().await;  // ← BLOCKS on sync!
let state = mgr.current_state.load();      // ← Never gets here during sync
```

**This is the critical insight:** Adding ArcSwap inside UtxoManager accomplishes nothing if readers still need to lock a Mutex to access it. **We must remove the outer Mutex entirely.**

### 2.2 Why In-Place Mutation Forces Blocking

Current code mutates the single UtxoManager in-place. There IS no "old" data - it's destroyed during the update.

---

## 3. Proposed Solution: Remove Outer Mutex + ArcSwap

### 3.1 Core Architecture Change

**Before (current):**
```
Daemon/Services
    ↓
Arc<Mutex<UtxoManager>>  ← ALL access requires lock
    ↓
UtxoManager { HashMap, Vec }
```

**After (proposed):**
```
Daemon/Services
    ↓
Arc<UtxoManager>  ← NO MUTEX for reads!
    ↓
UtxoManager {
    current_state: ArcSwap<UtxoState>  ← Atomic swap, lock-free load
}
    ↓
UtxoState { HashMap, Vec }  ← Immutable snapshots
```

### 3.2 Key Changes

1. **Remove outer Mutex** from all code:
   - `Arc<Mutex<UtxoManager>>` → `Arc<UtxoManager>`
   - All `.lock().await` calls removed for reads
   - Only sync needs to call methods, no locking needed

2. **Use ArcSwap inside UtxoManager**:
   - Stores current state as atomic Arc pointer
   - Readers call `.load()` - atomic, lock-free
   - Writers call `.store()` - atomic swap

3. **Extract state into UtxoState**:
   - Contains HashMap, Vec, mempool
   - Immutable after creation
   - Multiple versions can coexist via Arc

### 3.3 How This Solves The Problem

**Sync (writer):**
```rust
// Build new state (NO LOCKS, 2.3 seconds)
let new_state = build_new_utxo_state(...);

// Atomic swap (<1ms)
utxo_manager.current_state.store(Arc::new(new_state));
```

**Readers (concurrent with sync):**
```rust
// Load current state (atomic, lock-free, <1μs)
let state = utxo_manager.state();  // Returns Arc<UtxoState>

// Use it (old state remains valid during sync)
for (_, outpoint) in &state.utxo_keys_sorted_by_amount {
    let utxo = &state.utxos_by_outpoint[outpoint];
    // ...
}
```

**Timeline with fix:**
```
Time:     0ms        500ms       1000ms      1500ms      2000ms      2500ms
          |-----------|-----------|-----------|-----------|-----------|
Sync:     [build new state, no lock held........................][swap]
No Lock:  [==========READERS CONTINUE USING OLD STATE===================]

GetUtxos:     [reads old state................................][reads new]
GetBalance:        [reads old state......................][reads new]
SendTx:                 [reads old state.............][reads new]
```

---

## 4. Detailed Implementation

### 4.1 New Data Structures

**File:** `daemon/src/utxo_manager.rs`

```rust
use arc_swap::ArcSwap;
use std::sync::Arc;
use std::collections::HashMap;

/// The actual UTXO data - now immutable per version
///
/// IMPORTANT: This struct should NOT derive Clone - cloning 10M UTXOs is catastrophic.
/// State transitions happen by building new instances, not cloning.
pub struct UtxoState {
    /// Primary storage: outpoint -> UTXO
    pub utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,

    /// Sorted index for efficient traversal by amount
    /// Stores (amount, outpoint) tuples, sorted
    pub utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>,

    /// Metadata
    pub last_update_timestamp: std::time::Instant,
}

impl UtxoState {
    pub fn new_empty() -> Self {
        Self {
            utxos_by_outpoint: HashMap::new(),
            utxo_keys_sorted_by_amount: Vec::new(),
            last_update_timestamp: std::time::Instant::now(),
        }
    }

    pub fn utxo_count(&self) -> usize {
        self.utxos_by_outpoint.len()
    }
}

/// The manager holds an atomic pointer to current state
///
/// CRITICAL: This is used via Arc<UtxoManager>, NOT Arc<Mutex<UtxoManager>>
pub struct UtxoManager {
    address_manager: Arc<Mutex<AddressManager>>,
    coinbase_maturity: u64,

    /// Current UTXO state - atomically swappable, lock-free reads
    current_state: ArcSwap<UtxoState>,

    /// Mempool transactions handled separately (see section 5)
    mempool_transactions: Mutex<Vec<WalletSignableTransaction>>,
}
```

**Why ArcSwap instead of RwLock or Mutex?**
- `RwLock`: Readers still take a lock (even if shared)
- `Mutex`: Readers block exclusively
- `ArcSwap`: Readers are truly lock-free (atomic load)

**Dependencies:**
```toml
[dependencies]
arc-swap = "1.7"
```

### 4.2 Constructor

```rust
impl UtxoManager {
    pub fn new(
        address_manager: Arc<Mutex<AddressManager>>,
        consensus_params: Params,
        block_dag_info: GetBlockDagInfoResponse,
    ) -> Self {
        let coinbase_maturity = consensus_params
            .coinbase_maturity()
            .get(block_dag_info.virtual_daa_score);

        Self {
            address_manager,
            coinbase_maturity,
            current_state: ArcSwap::from_pointee(UtxoState::new_empty()),
            mempool_transactions: Mutex::new(Vec::new()),
        }
    }
}
```

### 4.3 Read Operations (Lock-Free!)

**CRITICAL: These do NOT require any Mutex on UtxoManager**

```rust
impl UtxoManager {
    /// Get current state snapshot
    ///
    /// This is lock-free - just an atomic pointer load.
    /// The returned Arc keeps the state alive even if sync swaps to new state.
    pub fn state(&self) -> Arc<UtxoState> {
        self.current_state.load_full()
    }

    /// Get UTXO count (convenience)
    pub fn utxo_count(&self) -> usize {
        self.state().utxo_count()
    }

    /// Get UTXO by outpoint (convenience)
    pub fn get_utxo_by_outpoint(&self, outpoint: &WalletOutpoint) -> Option<WalletUtxo> {
        self.state().utxos_by_outpoint.get(outpoint).cloned()
    }

    /// Check if UTXO is pending
    pub fn is_utxo_pending(&self, utxo: &WalletUtxo, virtual_daa_score: u64) -> bool {
        if !utxo.utxo_entry.is_coinbase {
            return false;
        }
        utxo.utxo_entry.block_daa_score + self.coinbase_maturity > virtual_daa_score
    }
}
```

**How callers use this:**

```rust
// OLD WAY (with Mutex):
let utxo_manager = self.utxo_manager.lock().await;  // Blocks on sync!
for utxo in utxo_manager.utxos_sorted_by_amount() {
    // ...
}

// NEW WAY (lock-free):
let state = self.utxo_manager.state();  // Atomic load, never blocks
for (_, outpoint) in &state.utxo_keys_sorted_by_amount {
    if let Some(utxo) = state.utxos_by_outpoint.get(outpoint) {
        // Use &WalletUtxo reference
        if self.utxo_manager.is_utxo_pending(utxo, daa_score) {
            continue;
        }
        // ...
    }
}
```

**Why this pattern works:**
- `state` holds an `Arc<UtxoState>`, keeping data alive
- References `&WalletUtxo` are valid as long as `state` is in scope
- No cloning of UTXOs needed
- Standard Rust borrow checker enforces safety

### 4.4 Write Operation (Build New State)

```rust
impl UtxoManager {
    /// Update UTXO set by building entirely new state
    ///
    /// This does NOT require &mut self - we're not mutating UtxoManager,
    /// just building a new state and swapping the Arc pointer.
    ///
    /// Readers continue using old state during this entire method.
    pub async fn update_utxo_set(
        &self,  // ← Note: &self not &mut self, and NO MUTEX NEEDED
        rpc_utxo_entries: Vec<RpcUtxosByAddressesEntry>,
        rpc_mempool_utxo_entries: Vec<RpcMempoolEntryByAddress>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        // Build exclude set
        let mut exclude: HashSet<WalletOutpoint> = HashSet::new();
        for rpc_mempool_entries_by_address in &rpc_mempool_utxo_entries {
            for sending_rpc_mempool_entry in &rpc_mempool_entries_by_address.sending {
                for input in &sending_rpc_mempool_entry.transaction.inputs {
                    exclude.insert(input.previous_outpoint.into());
                }
            }
        }

        // Get address map
        let address_map: Arc<HashMap<Address, WalletAddress>>;
        {
            let address_manager = self.address_manager.lock().await;
            address_map = address_manager.monitored_address_map().await?;
        }

        // BUILD NEW STATE (NO LOCKS HELD - 2.3+ seconds at 10M scale)
        // Readers continue using old state during this entire build
        let mut new_utxos_by_outpoint = HashMap::new();
        let mut new_sorted_keys = Vec::new();

        new_utxos_by_outpoint.reserve(rpc_utxo_entries.len());
        new_sorted_keys.reserve(rpc_utxo_entries.len());

        // Process consensus UTXOs
        for rpc_utxo_entry in rpc_utxo_entries {
            let wallet_outpoint: WalletOutpoint = rpc_utxo_entry.outpoint.into();
            if exclude.contains(&wallet_outpoint) {
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
                    format!("UTXO address {} not found in wallet address_set", address)
                })?
                .clone();

            let wallet_utxo = WalletUtxo::new(
                wallet_outpoint.clone(),
                wallet_utxo_entry,
                wallet_address
            );

            new_utxos_by_outpoint.insert(wallet_outpoint.clone(), wallet_utxo);
            new_sorted_keys.push((amount, wallet_outpoint));
        }

        // Process mempool UTXOs
        for rpc_mempool_entry in rpc_mempool_utxo_entries {
            for receiving_rpc_mempool_entry in &rpc_mempool_entry.receiving {
                let transaction = &receiving_rpc_mempool_entry.transaction;
                let Some(transaction_verbose_data) = &transaction.verbose_data else {
                    continue;
                };
                for (i, output) in transaction.outputs.iter().enumerate() {
                    let Some(output_verbose_data) = &output.verbose_data else {
                        continue;
                    };
                    let address_string = output_verbose_data
                        .script_public_key_address
                        .address_to_string();
                    let address = Address::try_from(address_string.as_str())
                        .map_err(|err| format!("invalid address: {err}"))?;

                    let Some(wallet_address) = address_map.get(&address) else {
                        continue;
                    };

                    let wallet_outpoint = WalletOutpoint::new(
                        transaction_verbose_data.transaction_id,
                        i as u32
                    );

                    if exclude.contains(&wallet_outpoint) {
                        continue;
                    }

                    let utxo_entry = WalletUtxoEntry::new(
                        output.value,
                        output.script_public_key.clone(),
                        0,
                        false,
                    );

                    let utxo = WalletUtxo::new(
                        wallet_outpoint.clone(),
                        utxo_entry,
                        wallet_address.clone()
                    );

                    new_utxos_by_outpoint.insert(wallet_outpoint.clone(), utxo);
                    new_sorted_keys.push((output.value, wallet_outpoint));
                }
            }
        }

        // SORT THE NEW DATA (takes 2.3s for 10M, but NO LOCK HELD)
        new_sorted_keys.sort_unstable();

        // Create new state
        let new_state = UtxoState {
            utxos_by_outpoint: new_utxos_by_outpoint,
            utxo_keys_sorted_by_amount: new_sorted_keys,
            last_update_timestamp: std::time::Instant::now(),
        };

        // ATOMIC SWAP - this is instant (<1μs)
        // Old state remains valid for any readers still using it (via Arc)
        self.current_state.store(Arc::new(new_state));

        Ok(())
    }
}
```

**Key properties:**
- Takes `&self`, not `&mut self`
- No Mutex needed (readers call `.state()` which doesn't need Mutex either)
- Builds entirely new HashMap and Vec (doesn't mutate old)
- Sort happens without any synchronization
- Only the `store()` is atomic (sub-microsecond)

---

## 5. Mempool Update Strategy

### 5.0 Why We Use a Mempool (Wallet Perspective)

We track a **wallet-side mempool** so the wallet can behave correctly *before* transactions are confirmed:

- **Prevent double-spends:** If we create a spend, the inputs are effectively unavailable immediately (even though consensus UTXOs haven’t changed yet).
- **Show correct “available vs pending”:** Balance/UTXO views should reflect that some funds are locked by pending sends, and that some outputs (e.g., change) are pending.
- **Avoid UX glitches during sync:** The consensus refresh is periodic; mempool overlay keeps the wallet state coherent between refreshes.

This is a **local overlay**, not a replacement for the node’s mempool. The node remains the source of truth for acceptance/confirmation; the wallet uses mempool only to model *pending intent* and *pending effects*.

### 5.1 The Problem

**Team review identified:** My original proposal to clone entire state for mempool updates is catastrophic:

```rust
// WRONG - clones 10M UTXOs per mempool transaction!
pub async fn add_mempool_transaction(&self, transaction: &WalletSignableTransaction) {
    let old_state = self.current_state.load();
    let mut new_state = (**old_state).clone();  // ← 8GB+ clone!
    // ... apply transaction ...
    self.current_state.store(Arc::new(new_state));
}
```

At 10M UTXOs, this could be 8GB+ of cloning per transaction. Completely unviable.

### 5.2 Solution: Separate Mempool State

**Keep mempool transactions in separate structure with its own small Mutex:**

```rust
pub struct UtxoManager {
    current_state: ArcSwap<UtxoState>,

    /// Needed to map `kaspa_addresses::Address` -> `WalletAddress` efficiently.
    /// (Used when constructing a mempool overlay view.)
    address_manager: Arc<Mutex<AddressManager>>,

    /// Mempool transactions stored separately
    /// These are applied as an "overlay" on top of consensus state
    mempool_transactions: Mutex<Vec<WalletSignableTransaction>>,
}

impl UtxoManager {
    /// Add mempool transaction (user-initiated send)
    pub async fn add_mempool_transaction(&self, transaction: &WalletSignableTransaction) {
        let mut mempool = self.mempool_transactions.lock().await;
        mempool.push(transaction.clone());
        // No need to modify UTXO state - mempool is separate overlay
    }

    /// Apply mempool transactions as overlay when reading
    ///
    /// Returns a view that combines consensus state + mempool modifications
    ///
    /// NOTE: This takes brief locks on mempool_transactions and address_manager.
    /// If mempool grows large (100+ pending txs), consider using Arc<Vec<...>> + versioning.
    pub async fn state_with_mempool(
        &self,
    ) -> Result<UtxoStateView, Box<dyn Error + Send + Sync>> {
        let consensus_state = self.state();
        let mempool = self.mempool_transactions.lock().await.clone();

        // Get address map for efficient Address→WalletAddress lookup
        let address_map = {
            let address_manager = self.address_manager.lock().await;
            address_manager.monitored_address_map().await?
        };

        Ok(UtxoStateView::new(consensus_state, mempool, address_map))
    }

    /// During full refresh, clean up confirmed/invalid mempool txs
    pub async fn update_utxo_set(&self, ...) -> Result<...> {
        // ... build new state as before ...

        self.current_state.store(Arc::new(new_state));

        // Clean mempool: remove confirmed transactions
        let mut mempool = self.mempool_transactions.lock().await;
        mempool.retain(|tx| {
            // Keep only transactions that are still pending
            self.is_transaction_still_pending(tx, &new_state)
        });

        Ok(())
    }
}

/// View combining consensus state + mempool overlay
///
/// NOTE: Mempool UTXOs are NOT included in sorted iteration by default.
/// They appear in get_utxo() lookups but not in sorted_iter().
/// This matches typical wallet behavior: pending/mempool UTXOs shown separately.
pub struct UtxoStateView {
    consensus_state: Arc<UtxoState>,
    removed_utxos: HashSet<WalletOutpoint>,
    added_utxos: HashMap<WalletOutpoint, WalletUtxo>,
}

impl UtxoStateView {
    fn new(
        consensus_state: Arc<UtxoState>,
        mempool_txs: Vec<WalletSignableTransaction>,
        address_map: Arc<HashMap<Address, WalletAddress>>,
    ) -> Self {
        let mut removed_utxos = HashSet::new();
        let mut added_utxos = HashMap::new();

        // Apply mempool transactions as overlay
        for tx in &mempool_txs {
            let transaction = &tx.transaction.unwrap_ref().tx;

            // Inputs are removed from UTXO set
            for input in &transaction.inputs {
                removed_utxos.insert(input.previous_outpoint.into());
            }

            // Outputs are added to UTXO set
            // Map Address -> WalletAddress using prebuilt map (no string conversion!)
            for (i, output) in transaction.outputs.iter().enumerate() {
                let outpoint = WalletOutpoint {
                    transaction_id: transaction.id(),
                    index: i as u32,
                };

                // tx.address_by_output_index[i] is Address (kaspa address)
                // Look up WalletAddress (derivation index) from prebuilt map
                let kaspa_address = &tx.address_by_output_index[i];
                let Some(wallet_address) = address_map.get(kaspa_address) else {
                    // Not our wallet's address
                    continue;
                };

                let utxo_entry = WalletUtxoEntry {
                    amount: output.value,
                    script_public_key: output.script_public_key.clone(),
                    block_daa_score: 0,
                    is_coinbase: false,
                };

                let utxo = WalletUtxo::new(
                    outpoint.clone(),
                    utxo_entry,
                    wallet_address.clone()
                );
                added_utxos.insert(outpoint, utxo);
            }
        }

        Self {
            consensus_state,
            removed_utxos,
            added_utxos,
        }
    }

    /// Check if UTXO exists (considering mempool overlay)
    pub fn contains_utxo(&self, outpoint: &WalletOutpoint) -> bool {
        if self.removed_utxos.contains(outpoint) {
            return false;
        }
        if self.added_utxos.contains_key(outpoint) {
            return true;
        }
        self.consensus_state.utxos_by_outpoint.contains_key(outpoint)
    }

    /// Get UTXO (considering mempool overlay)
    pub fn get_utxo(&self, outpoint: &WalletOutpoint) -> Option<&WalletUtxo> {
        if self.removed_utxos.contains(outpoint) {
            return None;
        }
        if let Some(utxo) = self.added_utxos.get(outpoint) {
            return Some(utxo);
        }
        self.consensus_state.utxos_by_outpoint.get(outpoint)
    }

    /// Iterate consensus UTXOs in sorted order (mempool UTXOs excluded)
    ///
    /// Mempool UTXOs are pending/unconfirmed and typically shown separately.
    /// If you need to include them, use get_utxo() to check individual outpoints.
    pub fn sorted_utxos_iter(&self) -> impl Iterator<Item = &WalletUtxo> + '_ {
        self.consensus_state.utxo_keys_sorted_by_amount
            .iter()
            .filter_map(|(_, outpoint)| {
                // Skip if removed by mempool
                if self.removed_utxos.contains(outpoint) {
                    return None;
                }
                self.consensus_state.utxos_by_outpoint.get(outpoint)
            })
    }

    /// Get all UTXOs (consensus + mempool) - unsorted
    ///
    /// Returns consensus UTXOs not removed by mempool, plus mempool additions.
    /// For sorted iteration, use sorted_utxos_iter() (consensus only).
    pub fn all_utxos(&self) -> impl Iterator<Item = &WalletUtxo> + '_ {
        let consensus_iter = self.consensus_state.utxos_by_outpoint
            .iter()
            .filter(|(outpoint, _)| !self.removed_utxos.contains(outpoint))
            .map(|(_, utxo)| utxo);

        let mempool_iter = self.added_utxos.values();

        consensus_iter.chain(mempool_iter)
    }
}
```

### 5.3 Sorted Iteration with Overlay

**Challenge:** How do mempool UTXOs appear in sorted-by-amount iteration?

**Current behavior (from code):**
- GetUtxos and transaction selection iterate `utxo_keys_sorted_by_amount`
- This is rebuilt during sync and (today) includes:
  - confirmed UTXOs (from `get_utxos_by_addresses`)
  - receiving mempool outputs (from `get_mempool_entries_by_addresses(...).receiving`)
  - wallet-generated pending txs re-applied after refresh (local `mempool_transactions`)
- Mempool *spends* are handled by excluding spent outpoints; mempool *receives* can appear in the sorted list

**With overlay pattern:**

**Option 1: Mempool UTXOs excluded from sorted iteration (Recommended)**
```rust
impl UtxoStateView {
    /// Iterate consensus UTXOs only, in sorted order
    /// Mempool UTXOs shown separately as "pending"
    pub fn sorted_utxos_iter(&self) -> impl Iterator<Item = &WalletUtxo> + '_ {
        self.consensus_state.utxo_keys_sorted_by_amount
            .iter()
            .filter_map(|(_, outpoint)| {
                if self.removed_utxos.contains(outpoint) {
                    None  // Spent by pending tx
                } else {
                    self.consensus_state.utxos_by_outpoint.get(outpoint)
                }
            })
    }

    /// Get pending (mempool) UTXOs separately - unsorted
    pub fn pending_utxos(&self) -> impl Iterator<Item = &WalletUtxo> + '_ {
        self.added_utxos.values()
    }
}
```

**Pros:**
- ✅ Simple: consensus is sorted, mempool is separate
- ✅ Matches UX: wallets typically show "confirmed" vs "pending" separately
- ✅ No sorting needed on mempool updates
- ✅ Efficient: sorted iteration doesn't need to check overlay

**Cons:**
- ⚠️ API change: callers need to handle pending separately
- ⚠️ Transaction selection might miss pending UTXOs (may be desired behavior)

**Option 2: Merge sorted (Complex, not recommended)**
```rust
// Would need to:
// 1. Iterate consensus sorted list
// 2. Insert mempool UTXOs in sorted order during iteration
// 3. Complex merging logic, allocations
// Not recommended due to complexity and performance cost
```

**Recommendation:** Use Option 1. Most wallet UIs show pending transactions separately anyway. Transaction selection can explicitly query pending if needed.

### 5.4 Mempool Vec Cloning Hotspot

**Issue identified in review:**

```rust
// In state_with_mempool():
let mempool = self.mempool_transactions.lock().await.clone();  // Clones Vec
```

If mempool has 100 pending transactions, each `state_with_mempool()` call clones 100 transaction objects. For high-frequency calls, this could be expensive.

**When this matters:**
- Active wallet with many pending txs (>50)
- High request rate (>10 req/s calling state_with_mempool)
- Large transaction objects

**Mitigation Option A: Arc + Versioning (if needed)**

```rust
pub struct UtxoManager {
    current_state: ArcSwap<UtxoState>,

    // Versioned mempool with Arc (avoids cloning)
    mempool_state: ArcSwap<MempoolState>,
}

struct MempoolState {
    transactions: Vec<WalletSignableTransaction>,
    version: u64,
}

pub fn mempool(&self) -> Arc<MempoolState> {
    self.mempool_state.load_full()  // No clone
}
```

**Mitigation Option B: Accept the clone (if mempool stays small)**

If typical mempool is <20 transactions:
- Clone cost: ~20 × ~1KB = ~20KB per call
- Acceptable for most use cases
- Keep simple implementation

**Recommendation:**
- Start with simple clone approach
- Monitor mempool size in production
- If median mempool >50 txs, switch to Arc + versioning
- Document the trade-off

### 5.5 Trade-offs

**Pros:**
- ✅ Never clones entire UTXO state
- ✅ Mempool updates are fast (just append to Vec)
- ✅ Mempool lock is separate and small-scope
- ✅ Consensus state remains immutable

**Cons:**
- ⚠️ More complex read path (need to check overlay, map Address→WalletAddress)
- ⚠️ Mempool Mutex still blocks (but only for `state_with_mempool()`, not `state()`)
- ⚠️ Cloning mempool Vec on each read: if 100+ pending txs, consider Arc<Vec<...>>
- ⚠️ Mempool UTXOs not in sorted iteration (shown separately as "pending")

**Alternative approaches:**
1. **No ad-hoc mempool updates:** Only apply during full refresh (simplest, loses real-time pending view)
2. **Arc<Vec<...>> for mempool:** Version + Arc to avoid cloning on each read
3. **Persistent data structures:** Use `im` crate for structural sharing (slower, more memory)
4. **Separate pending query:** API explicitly separates confirmed vs pending UTXOs

**Recommendation:**
- Use overlay pattern if mempool is typically <20 pending txs
- If mempool grows large, switch to Arc<Vec<...>> + versioning
- Document that mempool overlay requires brief lock (not lock-free like consensus state)

**Key limitation to document:**
Consensus state reads are lock-free, but combining with mempool (`state_with_mempool()`) requires locking mempool Mutex. This is acceptable since most operations only need consensus state.

---

## 6. Memory & Performance Analysis

### 6.1 Memory Usage

⚠️ **IMPORTANT:** All numbers below are **rough estimates** requiring real measurement.

**Current approach (estimated):**
```
Single UtxoManager:
- WalletUtxo: ~80-200 bytes each (depends on Address, ScriptPublicKey sizes)
- HashMap overhead: ~50-100% (depends on load factor)
- Vec overhead: ~16 bytes per entry
- 10M UTXOs: ??? GB (MEASURE THIS!)

Estimate range: 4-12 GB (very rough!)
```

**New approach (estimated):**
```
During sync (worst case):
- Old state: ??? GB (still referenced by readers)
- New state being built: ??? GB
- Peak: ~2× steady state

After sync completes:
- Old state: dropped when last reader finishes
- New state: ??? GB
- Steady: same as current

Risk: Long-running readers (e.g., slow GetUtxos response) keep old state alive longer
```

**Critical unknowns:**
1. Actual size of `WalletUtxo` in memory
2. HashMap memory overhead at 10M entries
3. Memory fragmentation effects
4. How long readers typically hold state references

**Required before deployment:**
```rust
// Measure actual memory usage
#[test]
fn measure_utxo_memory() {
    let state = create_utxo_state_with_real_data(10_000_000);
    let mem_before = get_process_rss();
    let _state_arc = Arc::new(state);
    let mem_after = get_process_rss();
    println!("10M UTXOs: {} GB", (mem_after - mem_before) / 1_000_000_000);
}
```

### 6.2 Performance Comparison

**Lock contention (measured in time readers are blocked):**

| Operation | Current | With ArcSwap | Notes |
|-----------|---------|--------------|-------|
| Sync lock time | 2300ms | 0ms (atomic swap) | ∞ improvement |
| **Consensus state reads (state())** | Blocked 23% | **Never blocked** | Lock-free ✅ |
| **Mempool overlay reads (state_with_mempool())** | Blocked 23% | **Brief locks (<5ms)** | Locks mempool + address_map ⚠️ |
| GetUtxos (no pending) | 0-2300ms | <1ms | Uses state() |
| GetUtxos (with pending) | 0-2300ms | 1-5ms | Uses state_with_mempool() |
| GetBalance | 0-2300ms | <1ms | Uses state() |
| P99 latency (overall) | ~2000ms | <10ms | ~200× improvement |

**Critical distinction:**
- `state()` - Truly lock-free, consensus UTXOs only ✅
- `state_with_mempool()` - Takes locks, includes pending ⚠️

Most operations use consensus state only → get full lock-free benefit.

**CPU usage (estimated):**
- Sync: Same (still sorts 10M items)
- Readers: Slightly higher (Arc load + clone overhead)
- Overall: <1% increase

**Memory bandwidth:**
- Copying 10M pointers during build: ~80MB (acceptable)
- Arc reference counting: Negligible

### 6.3 At Different Scales

| UTXOs | Steady Memory | Peak Memory | Sync Lock Time (current) | Sync Lock Time (new) |
|-------|---------------|-------------|--------------------------|----------------------|
| 100K  | ~0.1 GB | ~0.2 GB | 23ms | <1μs |
| 1M    | ~0.8 GB | ~1.6 GB | 200ms | <1μs |
| 10M   | ~8 GB (?) | ~16 GB (?) | 2300ms | <1μs |
| 100M  | ~80 GB (?) | ~160 GB (?) | 26s | <1μs |

**Risk assessment:** Peak memory doubling is the main concern. At 10M UTXOs:
- If steady state is 12GB, peak could be 24GB
- If machine has 32GB total, this could cause swapping or OOM
- **Must measure and test with real data before deploying**

---

## 7. Migration Path

### 7.1 The Breaking Change

**This is NOT a local refactoring.** Removing the outer Mutex requires updating ALL code that accesses UtxoManager.

**Files that need changes:**
1. `daemon/src/sync_manager.rs` - Remove Mutex wrapper
2. `daemon/src/daemon.rs` - Pass Arc not Arc<Mutex>
3. `daemon/src/service/get_utxos.rs` - Use `.state()` instead of `.lock()`
4. `daemon/src/service/get_balance.rs` - Use `.state()` instead of `.lock()`
5. `daemon/src/transaction_generator.rs` - Use `.state()` instead of `.lock()`
6. Any tests that construct UtxoManager

### 7.2 Phase 1: Prepare Data Structures

**Add new code alongside old:**

```rust
// daemon/src/utxo_manager.rs

// NEW: Add UtxoState struct
pub struct UtxoState {
    pub utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,
    pub utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>,
    pub last_update_timestamp: std::time::Instant,
}

// KEEP OLD: UtxoManager with old fields (temporarily)
pub struct UtxoManager {
    address_manager: Arc<Mutex<AddressManager>>,
    coinbase_maturity: u64,

    // OLD fields (keep for now)
    utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,
    utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>,
    mempool_transactions: Vec<WalletSignableTransaction>,

    // NEW field (add)
    current_state_v2: ArcSwap<UtxoState>,
}
```

### 7.3 Phase 2: Add New Methods

```rust
impl UtxoManager {
    // NEW: State accessor
    pub fn state(&self) -> Arc<UtxoState> {
        self.current_state_v2.load_full()
    }

    // NEW: Update without lock
    pub async fn update_utxo_set_v2(&self, ...) -> Result<...> {
        // Implementation from section 4.4
    }

    // OLD: Keep existing methods temporarily
    pub async fn update_utxo_set(&mut self, ...) -> Result<...> {
        // ... existing code ...
    }
}
```

### 7.4 Phase 3: Update Sync Manager

```rust
// daemon/src/sync_manager.rs

pub struct SyncManager {
    // CHANGE: Remove Mutex wrapper
    utxo_manager: Arc<UtxoManager>,  // Was: Arc<Mutex<UtxoManager>>
    // ...
}

async fn refresh_utxos(&self) -> Result<...> {
    // ... RPC calls ...

    // OLD WAY (commented out):
    // let mut utxo_manager = self.utxo_manager.lock().await;
    // utxo_manager.update_utxo_set(...).await?;

    // NEW WAY (no lock):
    self.utxo_manager.update_utxo_set(...).await?;

    Ok(())
}
```

### 7.5 Phase 4: Update Service Methods

**Example: get_utxos.rs**

```rust
// OLD WAY:
pub async fn get_utxos(&self, request: GetUtxosRequest) -> WalletResult<...> {
    // ... setup ...

    let utxo_manager = self.utxo_manager.lock().await;  // ← REMOVE
    for utxo in utxo_manager.utxos_sorted_by_amount() {
        // ...
    }
}

// NEW WAY:
pub async fn get_utxos(&self, request: GetUtxosRequest) -> WalletResult<...> {
    // ... setup ...

    let state = self.utxo_manager.state();  // ← Lock-free
    for (_, outpoint) in &state.utxo_keys_sorted_by_amount {
        if let Some(utxo) = state.utxos_by_outpoint.get(outpoint) {
            // Use &WalletUtxo reference
            // ...
        }
    }
}
```

**Example: transaction_generator.rs**

```rust
// OLD WAY:
pub async fn select_utxos(
    &mut self,
    utxo_manager: &MutexGuard<'_, UtxoManager>,  // ← REMOVE MutexGuard
    // ...
) -> WalletResult<...> {
    for utxo in utxo_manager.utxos_sorted_by_amount() {
        // ...
    }
}

// NEW WAY:
pub async fn select_utxos(
    &mut self,
    utxo_manager: &UtxoManager,  // ← Direct reference
    // ...
) -> WalletResult<...> {
    let state = utxo_manager.state();
    for (_, outpoint) in &state.utxo_keys_sorted_by_amount {
        if let Some(utxo) = state.utxos_by_outpoint.get(outpoint) {
            // ...
        }
    }
}
```

### 7.6 Phase 5: Remove Old Code

After all callers updated:
1. Remove old fields from UtxoManager
2. Remove `_v2` suffixes
3. Update tests
4. Remove old imports

### 7.7 Rollback Strategy

**Problem:** This is a breaking change that's hard to rollback midway.

**Mitigation:**
1. Feature flag at build time:
   ```rust
   #[cfg(feature = "lock-free-utxo")]
   type UtxoManagerRef = Arc<UtxoManager>;

   #[cfg(not(feature = "lock-free-utxo"))]
   type UtxoManagerRef = Arc<Mutex<UtxoManager>>;
   ```

2. Parallel testing:
   - Run both implementations in test environment
   - Compare results for consistency
   - Measure performance of both

3. Gradual deployment:
   - Deploy to test nodes first
   - Monitor memory usage carefully
   - Canary deployment to 1% of production
   - Watch for OOM errors

### 7.8 Estimated Effort

| Phase | Effort | Risk |
|-------|--------|------|
| Add data structures | 2-4 hours | Low |
| Update sync manager | 2-4 hours | Medium |
| Update all services | 8-16 hours | Medium |
| Update tests | 4-8 hours | Low |
| Testing & validation | 16-32 hours | High |
| **Total** | **~1-2 weeks** | **Medium-High** |

---

## 8. RwLock Alternative

### 8.1 Simpler Approach: RwLock + Two-Phase Update

**The team review correctly noted:** RwLock CAN help if you only hold it during swap.

**Pattern:**
```rust
pub struct UtxoManager {
    // Store Arc inside RwLock - swap pointer, not whole struct
    state: RwLock<Arc<UtxoState>>,
}

pub async fn update_utxo_set(&self, ...) -> Result<...> {
    // Phase 1: Build new state (NO LOCK, 2.3 seconds)
    let new_state = Arc::new(build_new_state(...));

    // Phase 2: Swap Arc pointer (BRIEF WRITE LOCK, <1ms)
    let mut guard = self.state.write().await;
    *guard = new_state;  // Just swapping Arc, not copying data
    // Lock released

    Ok(())
}

pub async fn state(&self) -> Arc<UtxoState> {
    let guard = self.state.read().await;  // Shared lock
    Arc::clone(&*guard)  // Clone the Arc (cheap), not the data
}

pub async fn get_utxo(&self, outpoint: &WalletOutpoint) -> Option<WalletUtxo> {
    let state = self.state().await;  // Get Arc
    state.utxos_by_outpoint.get(outpoint).cloned()
}
```

### 8.2 RwLock vs ArcSwap Comparison

| Aspect | RwLock<Arc<T>> | ArcSwap<T> |
|--------|--------|---------|
| Read locking | Shared lock on RwLock, then Arc clone | No lock (atomic Arc load) |
| Write locking | Exclusive lock to swap Arc | Atomic Arc store |
| Readers during swap | Block on write lock (<1ms) | Never block |
| Memory overhead | RwLock + Arc | Just Arc |
| Complexity | Moderate (lock + Arc) | Moderate (just Arc) |
| Performance | Good (99.9% availability) | Best (~100% availability) |
| Read cost | Lock acquire + Arc clone (~10-100ns) | Atomic Arc load (~5ns) |

### 8.3 When to Use Each

**Use RwLock if:**
- ✅ Simpler implementation acceptable
- ✅ 99.99% availability good enough
- ✅ Want to minimize memory overhead
- ✅ Team less comfortable with ArcSwap

**Use ArcSwap if:**
- ✅ Need truly lock-free reads
- ✅ Want maximum performance
- ✅ 2× peak memory acceptable
- ✅ Team comfortable with Arc patterns

### 8.4 Recommendation

**For this project:**
- If targeting <5M UTXOs: **RwLock is probably sufficient**
- If targeting >5M UTXOs: **ArcSwap is worth the complexity**
- Either way: **Remove outer Mutex** and use two-phase update

**Start with RwLock, migrate to ArcSwap if needed.**

---

## 9. Testing Strategy

### 9.1 Unit Tests

**Note on examples:** This section shows the **ArcSwap** variant where `state()` is a sync, lock-free accessor. If you implement the **RwLock<Arc<_>>** variant from §8, replace `manager.state()` with `manager.state().await` in the snippets below.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_readers_not_blocked_during_sync() {
        let manager = Arc::new(create_test_manager_with_utxos(100_000).await);

        // Start long-running sync
        let mgr_clone = manager.clone();
        let sync_handle = tokio::spawn(async move {
            let updates = generate_large_update();
            mgr_clone.update_utxo_set(updates, vec![]).await.unwrap();
        });

        // While sync runs, do many reads
        let mut read_handles = vec![];
        for _ in 0..100 {
            let mgr_clone = manager.clone();
            let handle = tokio::spawn(async move {
                let state = mgr_clone.state();  // ArcSwap: should not block on sync
                assert!(state.utxo_count() > 0);
            });
            read_handles.push(handle);
        }

        // All reads should complete quickly
        for handle in read_handles {
            let result = tokio::time::timeout(
                Duration::from_millis(100),
                handle
            ).await;
            assert!(result.is_ok(), "Read blocked too long");
        }

        sync_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_state_consistency() {
        let manager = create_test_manager_with_utxos(1000).await;

        let state = manager.state();

        // HashMap and sorted Vec must have same count
        assert_eq!(
            state.utxos_by_outpoint.len(),
            state.utxo_keys_sorted_by_amount.len()
        );

        // Every sorted entry must exist in HashMap
        for (amount, outpoint) in &state.utxo_keys_sorted_by_amount {
            let utxo = state.utxos_by_outpoint.get(outpoint)
                .expect("Sorted key not in HashMap");
            assert_eq!(utxo.utxo_entry.amount, *amount);
        }

        // Sorted list must actually be sorted
        let amounts: Vec<_> = state.utxo_keys_sorted_by_amount
            .iter()
            .map(|(amt, _)| *amt)
            .collect();
        assert!(amounts.windows(2).all(|w| w[0] <= w[1]));
    }

    #[tokio::test]
    async fn test_arc_keeps_old_state_alive() {
        let manager = Arc::new(create_test_manager_with_utxos(100).await);

        // Get old state
        let old_state = manager.state();
        let old_count = old_state.utxo_count();

        // Update to new state
        manager.update_utxo_set(generate_different_utxos(), vec![]).await.unwrap();

        // Get new state
        let new_state = manager.state();
        let new_count = new_state.utxo_count();

        // Old and new should be different
        assert_ne!(old_count, new_count);

        // Old state should still be valid (Arc keeps it alive)
        assert_eq!(old_state.utxo_count(), old_count);

        // Can still iterate old state
        for (_, outpoint) in &old_state.utxo_keys_sorted_by_amount {
            assert!(old_state.utxos_by_outpoint.contains_key(outpoint));
        }
    }
}
```

### 9.2 Memory Leak Tests

```rust
#[tokio::test]
async fn test_no_memory_leak() {
    let manager = Arc::new(create_test_manager().await);

    // Force GC baseline
    tokio::task::yield_now().await;
    let baseline_rss = get_process_rss();

    // Do many sync cycles
    for _ in 0..100 {
        let updates = generate_update(10_000);
        manager.update_utxo_set(updates, vec![]).await.unwrap();

        // Ensure old states can be dropped
        tokio::task::yield_now().await;
    }

    // Force GC
    tokio::task::yield_now().await;
    let final_rss = get_process_rss();

    // Memory shouldn't grow unbounded
    // Allow 50% growth for caches, etc.
    assert!(
        final_rss < baseline_rss * 1.5,
        "Possible memory leak: {} -> {} bytes",
        baseline_rss,
        final_rss
    );
}
```

### 9.3 Load Testing

```rust
#[ignore]  // Run manually with --release
#[tokio::test]
async fn stress_test_concurrent_load() {
    let manager = Arc::new(create_manager_with_utxos(1_000_000).await);

    // Spawn continuous sync
    let mgr = manager.clone();
    let sync_handle = tokio::spawn(async move {
        for _ in 0..100 {
            mgr.update_utxo_set(generate_realistic_updates(), vec![])
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    // Spawn many concurrent readers
    let mut handles = vec![];
    for _ in 0..1000 {
        let mgr = manager.clone();
        let handle = tokio::spawn(async move {
            for _ in 0..1000 {
                let state = mgr.state();
                let _count = state.utxo_count();
                // Simulate some work
                tokio::time::sleep(Duration::from_micros(100)).await;
            }
        });
        handles.push(handle);
    }

    // All should complete without blocking
    for handle in handles {
        handle.await.unwrap();
    }
    sync_handle.await.unwrap();
}
```

### 9.4 Real-World Measurement Tests

```rust
#[test]
fn measure_actual_utxo_memory() {
    // Create realistic UTXOs with actual data
    let mut utxos = HashMap::new();
    for i in 0..10_000_000 {
        let outpoint = WalletOutpoint::new(Hash::from_bytes([i as u8; 32]), 0);
        let utxo = create_realistic_utxo(i);
        utxos.insert(outpoint, utxo);
    }

    let mem_usage = get_process_rss();
    println!("10M UTXOs: {:.2} GB", mem_usage as f64 / 1_000_000_000.0);

    // Drop and measure to confirm cleanup
    drop(utxos);
    let after_drop = get_process_rss();
    println!("After drop: {:.2} GB freed",
             (mem_usage - after_drop) as f64 / 1_000_000_000.0);
}
```

---

## 10. Risks & Mitigations

### 10.1 Risk: Memory Exhaustion

**Risk:** Peak memory (2× steady state) causes OOM

**Likelihood:** Medium (depends on machine size vs UTXO count)

**Impact:** High (daemon crash, data loss if not handled)

**Mitigation:**
1. **Measure first:** Test with real data at target scale
2. **Set limits:** Add memory limit checks before sync
3. **Graceful degradation:** If memory low, skip sync or fail gracefully
4. **Monitor:** Alert on high memory usage
5. **Document requirements:** "Requires 2× UTXO set size in RAM"

```rust
async fn update_utxo_set(&self, ...) -> Result<...> {
    // Check available memory before building new state
    let available = get_available_memory()?;
    let estimated_needed = self.state().utxo_count() * BYTES_PER_UTXO * 2;

    if available < estimated_needed {
        return Err("Insufficient memory for UTXO refresh".into());
    }

    // Proceed with build...
}
```

### 10.2 Risk: Long-Lived Readers

**Risk:** Slow client keeps old state alive for minutes, preventing memory reclaim

**Likelihood:** Medium (slow network, large responses)

**Impact:** Medium (high memory usage but not fatal)

**Mitigation:**
1. **Timeout** long-running operations
2. **Pagination** for large GetUtxos responses
3. **Monitor:** Track Arc reference counts
4. **Document:** "Readers should not hold state for >30s"

### 10.3 Risk: Breaking Changes

**Risk:** Migration breaks existing functionality

**Likelihood:** Medium (any refactoring has risk)

**Impact:** High (service outage)

**Mitigation:**
1. **Extensive testing:** Unit, integration, load tests
2. **Staged rollout:** Test → canary → production
3. **Feature flag:** Can toggle between old/new
4. **Rollback plan:** Can revert if issues found
5. **Monitoring:** Track errors, latency, memory

### 10.4 Risk: Subtle Correctness Bugs

**Risk:** Arc reference management causes use-after-free or similar

**Likelihood:** Low (Rust prevents most of these)

**Impact:** High (data corruption, crashes)

**Mitigation:**
1. **Rust type system:** Enforces safety
2. **Code review:** Careful review of Arc usage
3. **Fuzzing:** Random concurrent operations
4. **Valgrind/MIRI:** Memory safety checking

### 10.5 Risk: Performance Not as Expected

**Risk:** ArcSwap overhead negates benefits

**Likelihood:** Low (proven pattern)

**Impact:** Medium (wasted effort)

**Mitigation:**
1. **Benchmark first:** Compare before/after
2. **Profile:** Find actual bottlenecks
3. **Fallback:** Can use simpler RwLock approach
4. **Measure:** Real production metrics

---

## 11. Decision Matrix

### 11.1 Go / No-Go Criteria

**IMPLEMENT if all true:**
- ✅ Targeting >1M UTXOs in production
- ✅ 23% reader blocking is unacceptable
- ✅ Machine has 2× UTXO set size RAM available
- ✅ Team can commit 1-2 weeks + ongoing maintenance
- ✅ Can test at scale before deploying

**DON'T IMPLEMENT if any true:**
- ❌ Staying under 500K UTXOs (current approach fine)
- ❌ Memory constrained (<2× headroom)
- ❌ Can't test at scale
- ❌ Team prefers simplicity over performance
- ❌ RPC architecture will be changed anyway (delta sync)

### 11.2 Recommended Path

Based on 10M UTXO target:

1. **Short term (1-2 weeks):**
   - Measure actual memory usage at scale
   - Implement RwLock + two-phase update (simpler)
   - Deploy and measure improvement

2. **Medium term (1-2 months):**
   - If RwLock insufficient, upgrade to ArcSwap
   - Implement mempool overlay
   - Full load testing

3. **Long term (3-6 months):**
   - Investigate delta sync from node
   - This would reduce sync time from 2.3s to <100ms
   - Then lock contention matters less

### 11.3 Success Metrics

**Quantitative:**
| Metric | Before | Target | Measured |
|--------|--------|--------|----------|
| P99 GetUtxos latency | ~2s | <100ms | ??? |
| Reader availability | 77% | >99.9% | ??? |
| Sync lock time | 2300ms | <1ms | ??? |
| Memory (steady) | ??? GB | Same | ??? |
| Memory (peak) | ??? GB | <2× | ??? |

**Qualitative:**
- ✅ No user complaints about freezing
- ✅ Predictable response times
- ✅ No OOM crashes
- ✅ Team comfortable maintaining code

---

## 12. Summary

### 12.1 Key Corrections from Team Review

The following critical issues were identified and fixed:

1. ✅ **Outer Mutex removal is mandatory** - Adding ArcSwap inside Mutex doesn't help; must change `Arc<Mutex<UtxoManager>>` → `Arc<UtxoManager>` throughout codebase

2. ✅ **Iterator pattern corrected** - Return `Arc<UtxoState>` accessor for callers to iterate, not impossible `Iterator<Item = &T>` with internal Arc

3. ✅ **Mempool cloning fixed** - Use overlay with separate Mutex, not clone-entire-state approach

4. ✅ **RwLock trade-off clarified** - Two-phase update works with RwLock too; ArcSwap's benefit is lock-free reads, not just shorter locks

5. ✅ **Memory estimates marked** - All numbers are placeholders requiring real measurement

### 12.2 Remaining Caveats

**Not all reads are lock-free:**
- `state()` - Lock-free, consensus UTXOs only ✅
- `state_with_mempool()` - Locks mempool Mutex + AddressManager Mutex briefly ⚠️

**Sorted iteration limitation:**
- Mempool UTXOs excluded from sorted iteration
- Shown separately as "pending"
- Acceptable for most wallet UX patterns

**Mempool Vec cloning:**
- Acceptable if <20 pending txs
- Monitor and upgrade to Arc if needed

### 12.3 Core Recommendation

**Implement two-phase update pattern:**
1. Build new state without locks (2.3s)
2. Brief lock/swap (<1ms)
3. Start with **RwLock** (simpler, lower risk)
4. Upgrade to **ArcSwap** if measurements show read lock contention

**Most important:**
- Remove outer `Arc<Mutex<UtxoManager>>` - this is non-negotiable
- All call sites must be updated (breaking change)
- Extensive testing required

**Realistic assessment:**
- Effort: 1-2 weeks of dev + testing
- Risk: Medium-high (breaking change)
- Benefit: 2300× better availability for consensus state reads
- Memory: 2× peak (must measure and test)

### 12.4 Critical Implementation Notes

**Before starting, acknowledge these requirements:**

1. **Outer Mutex removal is breaking:**
   - Every `.lock().await` on utxo_manager must be removed
   - Signature changes: `&MutexGuard<UtxoManager>` → `&UtxoManager`
   - All call sites across ~5-6 files need updates
   - Not a local refactoring

2. **Async fn required for overlay:**
   - `state_with_mempool()` must be `async fn` (locks mempool + AddressManager briefly)
   - Can't return `impl Iterator` from async fn easily
   - Callers must await

3. **Address→WalletAddress mapping needed:**
   - WalletSignableTransaction has Vec<Address> (kaspa addresses)
   - WalletUtxo requires WalletAddress (derivation index)
   - Use cached `AddressManager::monitored_address_map()` (Address → WalletAddress)
   - Lock AddressManager only long enough to obtain the cached `Arc<HashMap<...>>`; do NOT hold it while iterating mempool outputs

4. **Sorted iteration limitation:**
   - Mempool UTXOs can't participate in sorted iteration without expensive merging
   - Document: consensus sorted, mempool separate (standard wallet pattern)
   - Callers expecting sorted pending UTXOs need API changes

5. **Mempool cloning is a trade-off:**
   - Cloning Vec<WalletSignableTransaction> on each state_with_mempool() call
   - Acceptable if <20 pending, expensive if >50
   - Monitor and upgrade to Arc<Vec<...>> if needed

6. **Not all reads are lock-free:**
   - `state()` - truly lock-free ✅
   - `state_with_mempool()` - locks mempool + address_manager ⚠️
   - Don't oversell as "zero blocking" - it's "no blocking on consensus state reads"

### 12.5 Next Steps

- [ ] Measure actual memory usage with 10M UTXOs (REQUIRED before deciding)
- [ ] Prototype RwLock approach first (lower risk, simpler)
- [ ] Test at scale in non-production environment
- [ ] Profile: Is RwLock's brief write lock acceptable? Or need ArcSwap?
- [ ] Implement mempool overlay with proper Address→WalletAddress mapping
- [ ] Update all call sites to remove Mutex wrapper
- [ ] Extensive integration testing (all services)
- [ ] Load testing with concurrent readers during sync
- [ ] Memory leak testing (verify Arc cleanup)
- [ ] Staged rollout with monitoring

---

## 13. Final Recommendation

### 13.1 What to Implement

**Phase 1: RwLock + Two-Phase Update (Recommended Start)**

Simpler, lower risk, solves 99% of the problem:

```rust
pub struct UtxoManager {
    // Store Arc inside RwLock - swap Arc pointer, not whole state
    state: RwLock<Arc<UtxoState>>,  // RwLock wraps Arc, not UtxoState directly!
    mempool: Mutex<Vec<WalletSignableTransaction>>,
    address_manager: Arc<Mutex<AddressManager>>,
    coinbase_maturity: u64,
}

// Used as: Arc<UtxoManager> (no outer Mutex!)

impl UtxoManager {
    pub async fn update_utxo_set(&self, ...) -> Result<...> {
        // Build new state (2.3s, no lock)
        let new_state = Arc::new(build_new_state(...)?);

        // Swap Arc pointer (brief write lock, <1ms)
        *self.state.write().await = new_state;  // Swaps Arc, not data

        // Clean mempool
        self.clean_mempool().await;

        Ok(())
    }

    pub async fn state(&self) -> Arc<UtxoState> {
        let guard = self.state.read().await;  // Brief read lock
        Arc::clone(&*guard)  // Clone Arc pointer (cheap), not data
    }
}
```

**Why RwLock<Arc<T>> not RwLock<T>:**
- ✅ Swapping Arc is cheap (just a pointer)
- ✅ Readers get Arc cheaply (just clone pointer)
- ❌ RwLock<T> would require cloning entire T (catastrophic at 10M UTXOs)

**Trade-offs:**
- ✅ Much simpler than ArcSwap (standard library)
- ✅ Readers block <1ms during swap (vs 2300ms currently)
- ✅ 99.9% availability (vs 77% currently)
- ⚠️ Still brief blocking during read lock acquire
- ⚠️ Two levels of indirection (RwLock + Arc)

**Phase 2: Upgrade to ArcSwap (If Needed)**

Only if profiling shows read lock contention:

```rust
pub struct UtxoManager {
    state: ArcSwap<UtxoState>,  // True lock-free
    // ... rest same as RwLock version
}

impl UtxoManager {
    pub fn state(&self) -> Arc<UtxoState> {
        self.state.load_full()  // No lock, atomic
    }
}
```

**Additional benefit:**
- ✅ Truly lock-free reads
- ✅ 100% availability for consensus state

### 13.2 What NOT to Implement

❌ **Don't:** Add ArcSwap inside existing Arc<Mutex<UtxoManager>>
   - Doesn't help, outer Mutex still blocks everything

❌ **Don't:** Clone entire UtxoState for mempool updates
   - Catastrophic at 10M UTXOs

❌ **Don't:** Return `Iterator<Item = &WalletUtxo>` with internal Arc
   - Lifetime issues, won't compile

❌ **Don't:** Assume memory numbers without measuring
   - Must test with real data at scale

### 13.3 Success Criteria

**Must achieve:**
- P99 latency < 100ms (vs ~2s currently)
- No OOM crashes at 10M UTXOs
- Correct behavior (all tests pass)
- No memory leaks (stable over 1000+ syncs)

**Should achieve:**
- Reader availability >99% (vs 77%)
- Peak memory <2× steady state
- Sync time unchanged (~2.3s is acceptable given RPC dominates)

**Nice to have:**
- True lock-free reads (ArcSwap)
- Mempool Arc optimization (if >50 pending txs common)

---

## Appendix: Code Examples

### A.1 Complete Minimal Example (RwLock)

```rust
use tokio::sync::RwLock;
use std::sync::Arc;

pub struct UtxoState {
    pub utxos: HashMap<WalletOutpoint, WalletUtxo>,
    pub sorted: Vec<(u64, WalletOutpoint)>,
}

pub struct UtxoManager {
    state: Arc<RwLock<UtxoState>>,
}

impl UtxoManager {
    pub async fn update(&self, rpc_data: Vec<...>) -> Result<()> {
        // Phase 1: Build (no lock, 2.3s)
        let new_state = Self::build_state(rpc_data)?;

        // Phase 2: Swap (brief lock, <1ms)
        let mut guard = self.state.write().await;
        *guard = new_state;

        Ok(())
    }

    pub async fn read(&self) -> Vec<WalletUtxo> {
        let guard = self.state.read().await;
        guard.sorted.iter()
            .filter_map(|(_, outpoint)| guard.utxos.get(outpoint).cloned())
            .collect()
    }
}
```

### A.2 Complete Minimal Example (ArcSwap)

```rust
use arc_swap::ArcSwap;
use std::sync::Arc;

pub struct UtxoState {
    pub utxos: HashMap<WalletOutpoint, WalletUtxo>,
    pub sorted: Vec<(u64, WalletOutpoint)>,
}

pub struct UtxoManager {
    state: ArcSwap<UtxoState>,
}

impl UtxoManager {
    pub async fn update(&self, rpc_data: Vec<...>) -> Result<()> {
        // Build new state (no lock, 2.3s)
        let new_state = Self::build_state(rpc_data)?;

        // Atomic swap (no lock, <1μs)
        self.state.store(Arc::new(new_state));

        Ok(())
    }

    pub fn read(&self) -> Arc<UtxoState> {
        self.state.load_full()  // Lock-free
    }
}

// Caller
let state = manager.read();
for (_, outpoint) in &state.sorted {
    if let Some(utxo) = state.utxos.get(outpoint) {
        // Use &WalletUtxo
    }
}
```

---

## Appendix: Further Reading

- **arc-swap crate:** https://docs.rs/arc-swap/
- **RwLock docs:** https://docs.rs/tokio/latest/tokio/sync/struct.RwLock.html
- **Lock-free data structures:** "The Art of Multiprocessor Programming" by Herlihy & Shavit
- **Memory measurement in Rust:** https://github.com/koute/memory-profiler
