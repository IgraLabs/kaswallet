# Rusty-Kaspa RPC Opportunities for Wallet Sync Optimization

**Date:** 2026-02-03
**Source:** rusty-kaspa repository analysis
**Status:** Proposal - Implementation Opportunity

---

## Executive Summary

**Discovery:** Rusty-kaspa provides event-based subscription APIs that can **eliminate the need to poll `get_utxos_by_addresses` every sync cycle**.

**Key APIs Found:**
1. **`NotifyUtxosChanged`** - Real-time UTXO change notifications (added/removed)
2. **`NotifyVirtualChainChanged`** - Delta updates with chain progression
3. **`GetVirtualChainFromBlockV2`** - Incremental chain sync for catch-up
4. **`PruningPointUtxoSetOverride`** - Signal for UTXO set reset

**Potential Impact:**
- **Current:** Fetch 10M UTXOs (2GB) every 10 seconds
- **With subscriptions:** Receive only changes (~10K UTXOs = 2MB per update)
- **Performance:** 100-1000× less data transfer
- **Sync time:** 10-15s → 100-500ms
- **Architecture:** Event-driven instead of polling

---

## 1. Current Approach (Polling)

### 1.1 What We Do Today

**File:** `daemon/src/sync_manager.rs:97-146`

```rust
async fn refresh_utxos(&self) -> Result<...> {
    let monitored_addresses = address_manager.monitored_addresses().await?;

    // FETCH EVERYTHING - all UTXOs for all addresses
    let utxos = self.kaspa_client
        .get_utxos_by_addresses(monitored_addresses.as_ref().clone())
        .await?;

    let mempool = self.kaspa_client
        .get_mempool_entries_by_addresses(monitored_addresses.as_ref().clone(), true, true)
        .await?;

    // Rebuild entire UTXO set from scratch
    utxo_manager.update_utxo_set(utxos, mempool).await?;
}

// Called every 10 seconds in sync loop
```

### 1.2 Problems at Scale

**At 10M UTXOs, 1M addresses:**

| Operation | Time | Data Size |
|-----------|------|-----------|
| Serialize 10M UTXOs (node) | 3-5s | 2GB protobuf |
| Network transfer | 1-2s | 2GB |
| Deserialize (wallet) | 1-2s | 2GB → structs |
| Sort | 2.3s | In-memory |
| **Total** | **~10-15s** | **2GB transferred** |

**Every 10 seconds, regardless of whether anything changed!**

Typical churn:
- Active wallet: 0.1-1% of UTXOs change per cycle
- Inactive wallet: 0% change for minutes/hours

**We're transferring 100-1000× more data than needed.**

---

## 2. Available Subscription APIs

### 2.1 NotifyUtxosChanged - Real-Time UTXO Deltas

**The Primary Solution for Wallet Sync**

**Location:** `~/Source/personal/rusty-kaspa/rpc/core/src/api/rpc.rs:514-527`

**Proto Definition:** `~/Source/personal/rusty-kaspa/rpc/grpc/core/proto/rpc.proto:506-536`

#### API Structure

**Subscribe Request:**
```protobuf
message NotifyUtxosChangedRequest {
    repeated RpcAddress addresses = 1;  // Empty = all addresses
    RpcNotifyCommand command = 2;       // START or STOP
}
```

**Notification Payload:**
```protobuf
message UtxosChangedNotification {
    repeated RpcUtxosByAddressesEntry added = 1;     // New/created UTXOs
    repeated RpcUtxosByAddressesEntry removed = 2;   // Spent UTXOs
}

message RpcUtxosByAddressesEntry {
    RpcAddress address = 1;
    RpcOutpoint outpoint = 2;
    RpcUtxoEntry utxoEntry = 3;  // amount, scriptPubKey, blockDaaScore, isCoinbase
}
```

#### Scope System

**File:** `~/Source/personal/rusty-kaspa/notify/src/scope.rs:156-200`

```rust
pub enum UtxosChangedScope {
    Addresses(Arc<Vec<RpcAddress>>),  // Filter by specific addresses
    All,                              // All UTXO changes (expensive)
}
```

**Dynamic address updates:**
- Can start with subset of addresses
- Dynamically add/remove addresses via mutation commands
- Server maintains per-connection address index

#### How It Works

1. **Client subscribes:**
   ```rust
   client.notify_utxos_changed(addresses, RpcNotifyCommand::Start).await?;
   ```

2. **Node monitors UTXO changes:**
   - When blocks are accepted to virtual chain
   - When transactions enter/leave mempool
   - Filters by subscribed addresses

3. **Node sends notifications:**
   - Only sends changes (added/removed)
   - Only for addresses client subscribed to
   - Pushed in real-time (no polling)

4. **Client applies delta:**
   ```rust
   match notification {
       UtxosChangedNotification { added, removed } => {
           for entry in removed {
               utxo_manager.remove_utxo(&entry.outpoint);
           }
           for entry in added {
               utxo_manager.insert_utxo(entry.outpoint, entry.into());
           }
       }
   }
   ```

#### Benefits

**Data transfer reduction:**
- Typical: 0.1% churn = 10K UTXOs instead of 10M
- 2MB instead of 2GB per update
- **1000× less data**

**Latency improvement:**
- Push notifications: <100ms from block acceptance
- Polling: up to 10s lag
- **100× faster awareness**

**Server efficiency:**
- Node only computes/sends deltas
- Address filtering on server side
- Lower CPU/bandwidth for both sides

---

### 2.2 NotifyVirtualChainChanged - Chain Progression

**For tracking confirmations and reorganizations**

**Location:** `~/Source/personal/rusty-kaspa/rpc/core/src/api/rpc.rs:514-527`

**Proto:** `rpc.proto:332-357`

#### Notification Structure

```protobuf
message VirtualChainChangedNotification {
    repeated RpcHash removedChainBlockHashes = 1;   // Reorg: blocks removed
    repeated RpcHash addedChainBlockHashes = 2;     // New blocks in chain
    repeated RpcAcceptedTransactionIds acceptedTransactionIds = 3;
}

message RpcAcceptedTransactionIds {
    RpcHash acceptingBlockHash = 1;
    repeated RpcHash acceptedTransactionIds = 2;
}
```

#### Use Cases

1. **Confirmation tracking:**
   - Each added block increases confirmation depth
   - Can update UI "confirmations" counter in real-time

2. **Reorganization handling:**
   - `removedChainBlockHashes` indicates reorg
   - Wallet must unconfirm transactions in removed blocks
   - Re-evaluate UTXO validity

3. **Combined with NotifyUtxosChanged:**
   - UtxosChanged tells you WHAT changed
   - VirtualChainChanged tells you WHY (which blocks)
   - Together provide complete picture

---

### 2.3 PruningPointUtxoSetOverride - Critical Reset Signal

**Location:** `~/Source/personal/rusty-kaspa/rpc/grpc/core/proto/rpc.proto:648-684`

**Scope:** `~/Source/personal/rusty-kaspa/notify/src/scope.rs:245-259`

#### What It Is

During initial block download (IBD) or significant pruning point changes, the node's UTXO index can be reset. This notification signals that the UTXO set is being rebuilt.

**Wallet must:**
1. Listen for this notification
2. When received: invalidate all cached UTXOs
3. Re-fetch full UTXO set via `GetUtxosByAddresses`
4. Resume incremental updates

**Why this matters:**
- Prevents wallet from getting out of sync
- Rare event (only during IBD or major reorg)
- Must be handled correctly for data integrity

---

## 3. Proposed New Architecture

### 3.1 Event-Driven Sync (Recommended)

**Replace polling with subscriptions:**

```rust
pub struct SyncManager {
    kaspa_client: Arc<GrpcClient>,
    utxo_manager: Arc<UtxoManager>,  // Note: No Mutex!
    address_manager: Arc<Mutex<AddressManager>>,

    // New: track subscription state
    is_subscribed: AtomicBool,
    last_full_sync: Mutex<Option<Instant>>,
}

impl SyncManager {
    pub async fn start_event_driven_sync(self: Arc<Self>) -> Result<()> {
        // Initial full sync
        self.full_utxo_refresh().await?;

        // Subscribe to UTXO changes
        let addresses = self.get_monitored_addresses().await?;
        self.kaspa_client
            .notify_utxos_changed(addresses, RpcNotifyCommand::Start)
            .await?;

        // Subscribe to chain changes (for confirmations)
        self.kaspa_client
            .notify_virtual_chain_changed(
                UtxosChangedScope::default(),
                RpcNotifyCommand::Start
            )
            .await?;

        // Subscribe to pruning point overrides (critical!)
        self.kaspa_client
            .notify_pruning_point_utxo_set_override(RpcNotifyCommand::Start)
            .await?;

        self.is_subscribed.store(true, Relaxed);

        // Listen for notifications (non-blocking)
        tokio::spawn(async move {
            self.notification_loop().await
        });

        Ok(())
    }

    async fn notification_loop(&self) -> Result<()> {
        // Get notification stream from client
        let mut notifications = self.kaspa_client.notification_stream();

        while let Some(notification) = notifications.next().await {
            match notification {
                Notification::UtxosChanged(notif) => {
                    self.handle_utxos_changed(notif).await?;
                }
                Notification::VirtualChainChanged(notif) => {
                    self.handle_chain_changed(notif).await?;
                }
                Notification::PruningPointUtxoSetOverride(_) => {
                    warn!("UTXO index reset detected, performing full refresh");
                    self.full_utxo_refresh().await?;
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_utxos_changed(
        &self,
        notification: UtxosChangedNotification
    ) -> Result<()> {
        debug!(
            "UTXO delta: {} added, {} removed",
            notification.added.len(),
            notification.removed.len()
        );

        // Apply delta (NO FULL REBUILD!)
        self.utxo_manager.apply_utxo_delta(
            notification.added,
            notification.removed
        ).await?;

        Ok(())
    }

    async fn full_utxo_refresh(&self) -> Result<()> {
        // Fallback to current approach (for initial sync or after reset)
        let addresses = self.get_monitored_addresses().await?;
        let utxos = self.kaspa_client
            .get_utxos_by_addresses(addresses)
            .await?;

        self.utxo_manager.update_utxo_set(utxos, vec![]).await?;
        self.last_full_sync.lock().await.replace(Instant::now());

        Ok(())
    }
}
```

### 3.2 UtxoManager Delta Application

**New method needed:**

```rust
impl UtxoManager {
    /// Apply incremental UTXO changes (from NotifyUtxosChanged)
    ///
    /// This is MUCH faster than update_utxo_set for small deltas.
    /// Uses existing insert_utxo/remove_utxo which maintain sort order.
    pub async fn apply_utxo_delta(
        &self,
        added: Vec<RpcUtxosByAddressesEntry>,
        removed: Vec<RpcUtxosByAddressesEntry>,
    ) -> Result<()> {
        // Get address map for lookups
        let address_map = {
            let mgr = self.address_manager.lock().await;
            mgr.monitored_address_map().await?
        };

        // If using ArcSwap approach from LOCK-CONTENTION-SOLUTION.md:
        // We'd build a new state by cloning old state and applying delta
        // But that's expensive for large states...

        // If using RwLock approach:
        // We can actually modify in place with brief locks!

        // Remove spent UTXOs
        for removed_entry in removed {
            let outpoint: WalletOutpoint = removed_entry.outpoint.into();
            // Note: With double-buffer approach, this is tricky
            // Need to build new state from old + delta
        }

        // Add new UTXOs
        for added_entry in added {
            // ... similar ...
        }

        Ok(())
    }
}
```

**Challenge with immutable snapshots:**
- ArcSwap/double-buffer approach requires building new state
- For large states (10M UTXOs), cloning to apply small delta (100 changes) is wasteful
- **This is where BTreeMap or mutable state with RwLock makes more sense**

### 3.3 Architecture Trade-offs

**Approach A: Event-Driven + Mutable State (RwLock)**

```rust
pub struct UtxoManager {
    state: RwLock<UtxoState>,  // Mutable, can apply deltas directly
}

pub async fn apply_utxo_delta(&self, added: Vec<...>, removed: Vec<...>) {
    let mut state = self.state.write().await;  // Brief lock

    for entry in removed {
        // Remove from HashMap: O(1)
        state.utxos_by_outpoint.remove(&entry.outpoint);

        // Remove from sorted Vec: O(N) but N is current size
        let key = (entry.amount, entry.outpoint);
        if let Ok(pos) = state.sorted.binary_search(&key) {
            state.sorted.remove(pos);
        }
    }

    for entry in added {
        // Insert into HashMap: O(1)
        state.utxos_by_outpoint.insert(entry.outpoint.clone(), entry.into());

        // Insert into sorted Vec: O(N)
        let key = (entry.amount, entry.outpoint);
        let pos = state.sorted.binary_search(&key).unwrap_or_else(|p| p);
        state.sorted.insert(pos, key);
    }

    // Lock released - typically <10ms for 100 changes
}
```

**Pros:**
- ✅ Can apply small deltas efficiently
- ✅ No full state cloning
- ✅ Works well with event-driven updates

**Cons:**
- ⚠️ Vec::insert/remove are O(N) (but N is for insert/remove count, and you only briefly hold write lock)
- ⚠️ Readers block during delta application (but brief, <10ms typically)

**Approach B: Event-Driven + Immutable Snapshots (ArcSwap)**

For small deltas, clone-and-modify might still be faster than we think:

```rust
pub async fn apply_utxo_delta(&self, added: Vec<...>, removed: Vec<...>) {
    let old_state = self.current_state.load_full();

    // Clone the HashMap and Vec (expensive but one-time)
    let mut new_map = old_state.utxos_by_outpoint.clone();
    let mut new_sorted = old_state.sorted.clone();

    // Apply delta
    // ... same insert/remove logic ...

    // Swap to new state
    self.current_state.store(Arc::new(UtxoState {
        utxos_by_outpoint: new_map,
        sorted: new_sorted,
        ...
    }));
}
```

**Problem:** Cloning 10M-entry HashMap for 100 changes is wasteful.

**Solution:** Use persistent/immutable data structures:

```rust
use im::HashMap;  // Persistent HashMap with structural sharing
use im::Vector;   // Persistent Vector

pub struct UtxoState {
    utxos_by_outpoint: im::HashMap<WalletOutpoint, WalletUtxo>,
    // ... sorted index is harder with persistent structures
}
```

**Pros:**
- ✅ Clone is O(1) with structural sharing
- ✅ Readers never block
- ✅ Multiple versions coexist cheaply

**Cons:**
- ⚠️ `im` crate has 2-3× slower operations than std
- ⚠️ Sorted index still challenging
- ⚠️ Learning curve

**Approach C: Hybrid - Events for Deltas, Polling for Full Sync**

```rust
// Use events for normal updates (0-1000 changes)
// Fall back to full refresh if delta is large (>10K changes)

pub async fn handle_utxos_changed(&self, notif: UtxosChangedNotification) {
    let delta_size = notif.added.len() + notif.removed.len();

    if delta_size > 10_000 {
        // Large delta - do full refresh (cheaper than many inserts/removes)
        warn!("Large UTXO delta ({}), doing full refresh", delta_size);
        self.full_utxo_refresh().await?;
    } else {
        // Small delta - apply incrementally
        self.utxo_manager.apply_utxo_delta(notif.added, notif.removed).await?;
    }
}
```

---

### 2.4 NotifyVirtualChainChanged - Chain Delta Updates

**Location:** `~/Source/personal/rusty-kaspa/rpc/core/src/api/rpc.rs:514-527`

**Proto:** `rpc.proto:332-357`

#### What It Provides

```protobuf
message VirtualChainChangedNotification {
    repeated RpcHash removedChainBlockHashes = 1;  // Reorg
    repeated RpcHash addedChainBlockHashes = 2;    // New blocks
    repeated RpcAcceptedTransactionIds acceptedTransactionIds = 3;
}
```

#### Use Cases

**Option 1: UTXO calculation from transactions**

Instead of NotifyUtxosChanged, compute UTXO changes from transactions:

```rust
async fn handle_chain_changed(&self, notif: VirtualChainChangedNotification) {
    for block_txs in notif.acceptedTransactionIds {
        for tx_id in block_txs.acceptedTransactionIds {
            // Fetch transaction details
            let tx = self.client.get_transaction(tx_id).await?;

            // Check inputs/outputs for our addresses
            for input in tx.inputs {
                if self.our_addresses.contains(&input.address) {
                    self.remove_utxo(&input.previous_outpoint);
                }
            }

            for (i, output) in tx.outputs.iter().enumerate() {
                if self.our_addresses.contains(&output.address) {
                    let utxo = build_utxo_from_output(tx.id(), i, output);
                    self.insert_utxo(utxo);
                }
            }
        }
    }
}
```

**Pros:**
- ✅ Most granular control
- ✅ Can compute exact balance changes
- ✅ Good for detailed transaction history

**Cons:**
- ⚠️ Requires fetching transaction details (N RPCs)
- ⚠️ More complex logic
- ⚠️ Higher bandwidth if many transactions

**Option 2: Use for confirmation tracking only**

Simpler approach:

```rust
// Use NotifyUtxosChanged for UTXO set
// Use NotifyVirtualChainChanged just for tracking depth

async fn handle_chain_changed(&self, notif: VirtualChainChangedNotification) {
    let blocks_added = notif.addedChainBlockHashes.len();

    // All existing UTXOs gain `blocks_added` more confirmations
    // Update UI confirmation counters
    self.emit_confirmation_update(blocks_added);
}
```

**Recommendation:** Use NotifyUtxosChanged for UTXOs, NotifyVirtualChainChanged for confirmations.

---

## 4. Implementation Plan

### 4.1 Phase 1: Parallel Polling + Events (Low Risk)

Keep current polling, add event subscriptions in parallel:

```rust
pub async fn start_hybrid_sync(self: Arc<Self>) -> Result<()> {
    // Subscribe to events
    self.subscribe_to_events().await?;

    // Keep polling as backup
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(60));  // Slower now
        loop {
            interval.tick().await;
            // Full refresh as backup/validation
            self.full_utxo_refresh().await?;
        }
    });

    // Listen to events (primary mechanism)
    self.event_loop().await
}
```

**Validation:**
- Events update state in real-time
- Polling every 60s validates correctness
- Can compare event-driven state vs polled state
- Catch any drift or bugs

### 4.2 Phase 2: Events Primary, Polling Fallback

Once validated:

```rust
pub async fn start_sync(self: Arc<Self>) -> Result<()> {
    self.full_utxo_refresh().await?;  // Initial
    self.subscribe_to_events().await?;

    // Polling only for:
    // 1. Connection loss recovery
    // 2. Pruning point override
    // 3. Periodic validation (every 5 minutes)

    self.event_loop_with_recovery().await
}
```

### 4.3 Phase 3: Pure Event-Driven

Eliminate polling entirely:

```rust
pub async fn start_sync(self: Arc<Self>) -> Result<()> {
    self.full_utxo_refresh().await?;
    self.subscribe_to_events().await?;

    // Only events, no polling
    // Full refresh only on:
    // - Startup
    // - PruningPointUtxoSetOverride
    // - Reconnection after disconnect

    self.event_loop().await
}
```

---

## 5. Performance Projections

### 5.1 Current vs Event-Driven

**Scenario:** 10M UTXOs, 1M addresses, 0.1% churn per cycle (10K UTXO changes)

| Metric | Polling (current) | Event-Driven | Improvement |
|--------|-------------------|--------------|-------------|
| Data per update | 2GB (all UTXOs) | 2MB (deltas) | 1000× less |
| Sync time | 10-15s | 100-500ms | 20-100× faster |
| Network bandwidth | 2GB / 10s = 200MB/s | 2MB / event | 100× less |
| Latency | 0-10s (poll delay) | <100ms (push) | 100× faster |
| Server CPU | High (serialize 10M) | Low (serialize 10K) | 1000× less |

### 5.2 At Different Activity Levels

| Churn % | Changes | Polling Data | Event Data | Improvement |
|---------|---------|--------------|------------|-------------|
| 0% (idle) | 0 | 2GB | 0 bytes | ∞ |
| 0.01% | 1K | 2GB | 200KB | 10,000× |
| 0.1% | 10K | 2GB | 2MB | 1,000× |
| 1% | 100K | 2GB | 20MB | 100× |
| 10% | 1M | 2GB | 200MB | 10× |

**Even at 10% churn (extreme), events are 10× better.**

### 5.3 Lock Contention Impact

**If combined with double-buffer approach:**

**Current:**
- Poll every 10s
- Lock for 2.3s (sort)
- 23% unavailable

**Events + Small Deltas:**
- Apply 100 changes every few seconds
- Lock for <10ms (small delta)
- <0.1% unavailable

**Combined benefit:**
- No large sort needed (small deltas)
- No large RPC fetch (only deltas)
- Readers almost never blocked

---

## 6. Implementation Challenges

### 6.1 Challenge: Delta Application with Immutable Snapshots

**Problem:** ArcSwap/immutable approach requires cloning state to apply delta.

**Options:**

1. **Accept the clone for deltas <1K:**
   - Cloning 10M-entry HashMap for 100 changes might be faster than you think
   - Modern allocators are efficient
   - Benchmark before dismissing

2. **Use persistent data structures (im crate):**
   - `im::HashMap` has structural sharing
   - Clone is O(1) with shared structure
   - Slower operations (2-3×) but efficient cloning

3. **Hybrid: Mutable for deltas, snapshot for reads:**
   - Apply deltas to mutable working copy
   - Periodically snapshot to Arc for readers
   - Complex but potentially best of both worlds

4. **Use RwLock instead of ArcSwap:**
   - Allows mutable delta application
   - Readers block briefly during delta (<10ms)
   - Simpler than immutable approach

**Recommendation:** Start with #4 (RwLock) if using events. Small deltas don't justify complex immutability.

### 6.2 Challenge: Connection Management

**Issues:**
- Subscriptions lost on disconnect
- Need to resubscribe on reconnect
- Must detect missed updates

**Solution:**
```rust
async fn ensure_subscribed(&self) -> Result<()> {
    if !self.is_subscribed.load(Relaxed) {
        // Reconnect + full refresh
        self.full_utxo_refresh().await?;
        self.subscribe_to_events().await?;
        self.is_subscribed.store(true, Relaxed);
    }
    Ok(())
}

// In event loop:
match notifications.next().await {
    Some(notif) => { /* handle */ }
    None => {
        warn!("Notification stream ended, reconnecting");
        self.is_subscribed.store(false, Relaxed);
        self.ensure_subscribed().await?;
    }
}
```

### 6.3 Challenge: Address Set Changes

**Issue:** When wallet generates new addresses, need to update subscription.

**Current NotifyUtxosChanged behavior:**
- Subscribes to specific address list
- Doesn't automatically track new addresses

**Solution:**
```rust
pub async fn new_address(&self) -> Result<(String, WalletAddress)> {
    let (address_string, wallet_address) = self.address_manager
        .new_address()
        .await?;

    // Update subscription to include new address
    if self.sync_manager.is_event_driven() {
        self.sync_manager.add_address_to_subscription(&address_string).await?;
    }

    Ok((address_string, wallet_address))
}
```

**Note:** Check if rusty-kaspa supports dynamic address updates or requires resubscribe.

---

## 7. Recommendation

### 7.1 Immediate Next Steps

1. **Prototype event-driven sync:**
   - Implement NotifyUtxosChanged subscription
   - Apply deltas using existing insert_utxo/remove_utxo
   - Run in parallel with current polling
   - Compare state consistency

2. **Measure actual improvement:**
   - Benchmark delta application time
   - Measure network bandwidth reduction
   - Test with real node at scale

3. **Evaluate data structure options:**
   - Benchmark: RwLock + mutable state for deltas
   - Benchmark: Clone-on-write with std HashMap for small deltas
   - Benchmark: `im` crate persistent structures
   - Choose based on real measurements

### 7.2 Recommended Architecture

**For 10M+ UTXO scale:**

```
┌─────────────────────────────────────────────────┐
│  Wallet Sync Architecture (Event-Driven)       │
├─────────────────────────────────────────────────┤
│                                                 │
│  Initial Sync:                                  │
│  └─> GetUtxosByAddresses (full fetch)           │
│                                                 │
│  Continuous Updates:                            │
│  ├─> NotifyUtxosChanged (subscribe)             │
│  │   └─> Apply deltas as they arrive            │
│  ├─> NotifyVirtualChainChanged (confirmations)  │
│  └─> PruningPointUtxoSetOverride (reset signal) │
│                                                 │
│  Fallback/Recovery:                             │
│  ├─> Reconnect: full refresh + resubscribe      │
│  └─> Periodic validation: full refresh (hourly) │
│                                                 │
│  Data Structure:                                │
│  └─> RwLock<UtxoState> for mutable deltas       │
│      OR                                         │
│      ArcSwap<UtxoState> + im::HashMap           │
│                                                 │
└─────────────────────────────────────────────────┘
```

### 7.3 Expected Outcomes

**Performance:**
- Sync updates: 10-15s → 100-500ms (20-100× faster)
- Network bandwidth: 2GB/10s → 2MB/event (1000× reduction)
- Lock contention: 23% → <1% (with small deltas)

**Scalability:**
- Can handle 100M+ UTXOs (limited by memory, not sync time)
- Real-time updates (<100ms latency)
- Efficient at any scale (idle wallet uses almost no resources)

**Complexity:**
- Moderate increase (subscription management)
- Robust patterns (many wallets use this approach)
- Better than current polling at scale

---

## 8. Open Questions

### 8.1 For rusty-kaspa API

1. **Does NotifyUtxosChanged support dynamic address updates?**
   - Can we add addresses without resubscribing?
   - Or must we unsubscribe + resubscribe with new list?

2. **What happens during heavy reorgs?**
   - Does NotifyUtxosChanged handle chain reorganizations correctly?
   - Or do we need to combine with VirtualChainChanged for safety?

3. **Is there a notification sequence guarantee?**
   - Are notifications delivered in order?
   - Can we miss notifications during reconnect?

4. **What's the subscription limit?**
   - Can we subscribe to 1M addresses?
   - Is there a server-side limit?

### 8.2 For kaswallet Implementation

1. **Which data structure for delta application?**
   - RwLock + mutable (simpler)
   - ArcSwap + persistent structures (more complex, lock-free)
   - Hybrid (best of both?)

2. **How to handle missed notifications?**
   - Detect gaps and trigger full refresh?
   - Use sequence numbers or timestamps?

3. **Testing at scale:**
   - How to test with 10M UTXOs + events?
   - Simulate node disconnects, reorgs, etc.

---

## 9. Conclusion

**The rusty-kaspa RPC provides exactly what we need:** Event-based UTXO change notifications that eliminate the need to poll the full UTXO set every cycle.

**Key APIs:**
- ✅ `NotifyUtxosChanged` - Delta updates (added/removed)
- ✅ `NotifyVirtualChainChanged` - Chain progression
- ✅ `PruningPointUtxoSetOverride` - Reset signal

**Impact:** Potentially **100-1000× reduction** in data transfer and sync time.

**Next Steps:**
1. Prototype NotifyUtxosChanged integration
2. Measure actual improvement
3. Decide on data structure (RwLock vs ArcSwap + persistent)
4. Implement with fallback to polling
5. Gradually migrate to pure event-driven

**This should be the #1 priority for scaling to 10M+ UTXOs** - far more impactful than any in-memory algorithm optimization.
