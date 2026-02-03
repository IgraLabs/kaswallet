# Performance optimizations (2026-02-02, updated 2026-02-03)

This document describes the code changes made to address **two long-running performance issues**:

1) **Slowdowns as the wallet UTXO set grows large** (CPU + memory + lock contention).
2) **Slowdowns as the wallet accumulates many derived addresses (BIP32)**.

The implemented address optimizations correspond to the **first 4 points** from the BIP32/address perf discussion:

1. Stop rescanning from index `0` every sync tick.
2. Eliminate string round-trips in discovery/balance queries.
3. Make address derivation cheaper (no path string parsing; avoid per-derivation multisig key sorting).
4. Cache monitored addresses so refresh does not rebuild/parse each tick.

**Update (2026-02-03):** Implemented the lock-contention fix for large UTXO sets by converting `UtxoManager` to a snapshot-based design (`RwLock<Arc<UtxoState>>` + `UtxoStateView` overlay). This removes the outer `Arc<Mutex<UtxoManager>>` and prevents long reader stalls during sync refresh.

## Files touched

- Updated:
  - `Cargo.toml`
  - `client/src/client.rs`
  - `common/src/addresses.rs`
  - `common/src/model.rs`
  - `common/src/proto_convert.rs`
  - `daemon/Cargo.toml`
  - `daemon/src/address_manager.rs`
  - `daemon/src/args.rs`
  - `daemon/src/daemon.rs`
  - `daemon/src/service/broadcast.rs`
  - `daemon/src/service/common.rs`
  - `daemon/src/service/create_unsigned_transaction.rs`
  - `daemon/src/service/get_balance.rs`
  - `daemon/src/service/get_utxos.rs`
  - `daemon/src/service/kaswallet_service.rs`
  - `daemon/src/service/send.rs`
  - `daemon/src/sync_manager.rs`
  - `daemon/src/transaction_generator.rs`
  - `daemon/src/utxo_manager.rs`
- Added:
  - `PERF_OPTIMIZATIONS.md`
  - `daemon/benches/address_scaling.rs`
  - `daemon/benches/from_addresses_filter.rs`
  - `daemon/benches/utxo_scaling.rs`
  - `daemon/src/bin/kaswallet_stress_bench.rs`

---

# A) UTXO set scaling

## A0. Lock contention removal (UTXO snapshots)

### Summary

- Removed the outer `Arc<Mutex<UtxoManager>>` and replaced it with `Arc<UtxoManager>`.
- Moved the actual UTXO data into an immutable snapshot:
  - `UtxoState { utxos_by_outpoint, utxo_keys_sorted_by_amount }`
- `UtxoManager::update_utxo_set(...)` now:
  - builds a brand new `UtxoState` **without holding the UTXO lock**
  - swaps the `Arc<UtxoState>` under a brief `RwLock` write-lock
- Reader paths (`GetUtxos`, `GetBalance`, transaction creation) now snapshot once via:
  - `UtxoManager::state()` (consensus snapshot)
  - `UtxoManager::state_with_mempool()` (consensus + wallet-local overlay)
  and then iterate without being blocked by the full rebuild+sort work.
- Wallet-local pending transactions are stored separately (`mempool_transactions`) and applied as a lightweight overlay (`UtxoStateView`) instead of cloning the full state.

### Key code locations

- `daemon/src/daemon.rs` constructs `Arc<UtxoManager>`
- `daemon/src/sync_manager.rs` calls `utxo_manager.update_utxo_set(...)` without locking
- `daemon/src/utxo_manager.rs`
  - `UtxoState`, `UtxoStateView`
  - `UtxoManager::{state, state_with_mempool, update_utxo_set, add_mempool_transaction}`
- Services updated to use snapshots:
  - `daemon/src/service/get_utxos.rs`
  - `daemon/src/service/get_balance.rs`
  - `daemon/src/service/create_unsigned_transaction.rs`
  - `daemon/src/service/send.rs`
  - `daemon/src/service/broadcast.rs`

## A1. UTXO storage/indexing (`daemon/src/utxo_manager.rs`)

### Summary

- Keep UTXOs in an immutable snapshot:
  - `UtxoState::utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>`
- Maintain a lightweight sorted index by amount:
  - `UtxoState::utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>` sorted by `(amount, outpoint)`
- Expose sorted iteration on snapshots (no cloning):
  - `UtxoState::utxos_sorted_by_amount(&self) -> impl Iterator<Item = &WalletUtxo>`
  - `UtxoStateView::utxos_sorted_by_amount(&self) -> UtxosSortedByAmountIter<'_>` (merge iterator for wallet-local overlay)

### Why it helps

- Avoids storing the full `WalletUtxo` twice (sorted index stores only `(amount, outpoint)` keys).
- Makes “UTXOs sorted by amount” iteration deterministic and cheap (stable tie-breaker by outpoint).
- Works naturally with snapshot swapping + overlay view: readers iterate over a consistent snapshot without taking long locks or cloning the full set.

### Key code locations

- `daemon/src/utxo_manager.rs`
  - `UtxoState::utxos_sorted_by_amount`
  - `UtxoStateView::utxos_sorted_by_amount` + `UtxosSortedByAmountIter`
  - `UtxoManager::update_utxo_set` (rebuilds map + sorted key index; uses cached address map)

## A2. `GetUtxos` hot path (`daemon/src/service/get_utxos.rs`)

### Summary

- Avoid cloning the entire sorted UTXO list per request.
- Avoid per-UTXO async locking and per-UTXO mass estimation.
- Optimize address filtering:
  - Prebuild `allowed_addresses: Option<HashSet<String>>` when request filters by address.
  - Prebuild `wallet_address_to_string: HashMap<WalletAddress, String>` by inverting `address_set`.
- Compute a dust threshold once (single mass estimate), then dust-check is just a comparison.
- Serialize UTXOs without cloning:
  - `WalletUtxo::to_proto(...)` borrows `self`.

### Key code locations

- `daemon/src/service/get_utxos.rs` `get_utxos(...)`
- `common/src/proto_convert.rs` `WalletUtxo::to_proto(...)`

## A3. Sync loop lock scope (`daemon/src/sync_manager.rs`)

### Summary

- `SyncManager::refresh_utxos` performs RPC calls without holding any UTXO lock.
- `UtxoManager::update_utxo_set(...)` builds a new snapshot without holding the UTXO lock and swaps the `Arc` under a brief write-lock.

### Key code locations

- `daemon/src/sync_manager.rs` `refresh_utxos(...)`

## A4. Transaction selection paths (`daemon/src/transaction_generator.rs`)

### Summary

- Selection now operates on a snapshot/view (`UtxoStateView`) instead of locking `UtxoManager`.
- Avoid cloning the full UTXO set inside:
  - `select_utxos(...)` (iterates `utxo_state.utxos_sorted_by_amount()`)
  - `more_utxos_for_merge_transaction(...)` (iterates `utxo_state.utxos_sorted_by_amount()`)
- Avoid hashing full `WalletUtxo` values; use `WalletOutpoint` identity instead.
- Fix `from_addresses` filtering cost:
  - Prebuild `from_addresses_set: Option<HashSet<WalletAddress>>` once per call.
  - Per-UTXO filtering becomes O(1) average instead of O(M) slice scan.

### Key code locations

- `daemon/src/transaction_generator.rs` `select_utxos(...)`
- `daemon/src/transaction_generator.rs` `more_utxos_for_merge_transaction(...)`

---

# B) Address discovery scaling (many BIP32 addresses)

## B1. Point 1: Incremental recent-address scan (no rescan from 0 each tick)

### Summary

After the initial full scan, `collect_recent_addresses` now performs a **round-robin incremental scan**:

- Each sync cycle scans **one chunk** of size `NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES` and advances a cursor.
- Over time this covers the full range `[0, last_used_index + LOOKAHEAD)`.
- This preserves correctness (older previously-empty addresses are still revisited eventually), while avoiding an **O(N)** rescan from index `0` every tick.

### Key code locations

- `daemon/src/sync_manager.rs`
  - new field: `SyncManager::recent_scan_next_index`
  - `collect_recent_addresses(...)` chooses:
    - `collect_recent_addresses_full_scan(...)` on first sync
    - `collect_recent_addresses_incremental(...)` afterwards
  - helpers: `recent_scan_frontier(...)`, `recent_scan_step(...)`

## B2. Point 2: Eliminate string round-trips in discovery queries

### Summary

Address discovery no longer uses `String` address keys for querying balances.

- New type: `AddressQuerySet = HashMap<Address, WalletAddress>`
- `AddressManager::addresses_to_query(...)` returns `AddressQuerySet`
- `AddressManager::update_addresses_and_last_used_indexes(...)` consumes `AddressQuerySet` and removes entries using the `Address` key directly

This avoids repeated `to_string()` / parse cycles in the discovery hot path.

### Key code locations

- `daemon/src/address_manager.rs`
  - `AddressQuerySet`
  - `addresses_to_query(...)`
  - `update_addresses_and_last_used_indexes(...)`
- `daemon/src/sync_manager.rs` `collect_addresses(...)`

## B3. Point 3: Cheaper derivation + pre-sorted multisig keys

### Summary

Two derivation hot-path optimizations:

1) Avoid derivation path string formatting/parsing:
   - `AddressManager::calculate_address_path(...)` now builds a `DerivationPath` using `ChildNumber` instead of `format!(...)` + `parse()`.

2) Avoid per-derivation sorting for multisig:
   - Multisig extended public keys are sorted once in `AddressManager::new(...)`.
   - New helper `multisig_address_from_sorted_keys(...)` derives keys assuming they are already sorted.

### Key code locations

- `daemon/src/address_manager.rs`
  - `AddressManager::new(...)` sorts public keys once
  - `calculate_address_path(...)`
  - `multisig_address(...)` delegates to `multisig_address_from_sorted_keys(...)`
- `common/src/addresses.rs`
  - `multisig_address_from_sorted_keys(...)`
  - `multisig_address(...)` now sorts then delegates

## B4. Point 4: Cache monitored addresses for refresh

### Summary

`refresh_utxos` needs the full monitored address list every sync tick. Previously, that list was rebuilt (and strings parsed) repeatedly.

- `AddressManager` now maintains a versioned cache:
  - `address_set_version: AtomicU64`
  - `monitored_addresses_cache: Mutex<...>`
- New method: `AddressManager::monitored_addresses() -> Result<Arc<Vec<Address>>, ...>`
  - Rebuilds only when the version changes.
  - Returns an `Arc<Vec<Address>>` so callers can cheaply reuse it.
- Added: `AddressManager::monitored_address_map() -> Result<Arc<HashMap<Address, WalletAddress>>, ...>`
  - Same cache + invalidation, but returns a fast lookup map used by `UtxoManager::update_utxo_set` (avoids cloning the full address set).
- Cache invalidation occurs on:
  - `new_address(...)`
  - `change_address(...)`
  - `update_addresses_and_last_used_indexes(...)` (when it inserts any new addresses)

`SyncManager::refresh_utxos(...)` now uses this cached list and includes an early return for an empty wallet (avoid unnecessary RPC).

### Key code locations

- `daemon/src/address_manager.rs`
  - `AddressManager::monitored_addresses(...)`
  - `AddressManager::monitored_address_map(...)`
  - `address_set_version` increments on address set changes
- `daemon/src/sync_manager.rs`
  - `refresh_utxos(...)` uses `monitored_addresses()`
- `daemon/src/utxo_manager.rs`
  - `update_utxo_set(...)` uses `monitored_address_map()`

---

# C) Tests added/updated

- `daemon/src/sync_manager.rs`
  - Tests for incremental scan math:
    - `recent_scan_frontier_saturates_at_u32_max`
    - `recent_scan_step_clamps_end_to_frontier_and_wraps`
    - `recent_scan_step_advances_cursor_in_chunks_and_wraps`
    - `recent_scan_step_resets_stale_cursor_to_zero`
    - `recent_scan_step_zero_frontier_is_empty`
- `daemon/src/address_manager.rs`
  - `calculate_address_path_singlesig_matches_expected_format`
  - `addresses_to_query_uses_address_keys_and_expected_count`
  - `monitored_addresses_cache_is_reused_and_invalidated_on_change`
- `common/src/addresses.rs`
  - `multisig_address_sorted_helper_matches_existing_function`
- `daemon/src/utxo_manager.rs`
  - `update_utxo_set_produces_sorted_index`
  - `state_snapshots_remain_valid_after_update`
  - `state_with_mempool_overlays_wallet_transactions`
- `daemon/src/args.rs`
  - `sync_interval_default_matches_clap_default`

# D) How to validate

In this environment, builds/tests were run with `RUSTC_WRAPPER=` to avoid the configured sccache wrapper:

```bash
RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo test -q
```

# D2) Benchmarks

Criterion benchmarks were added to make performance regressions measurable (and to quantify improvements locally).

Run them with:

```bash
RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo bench -p kaswallet-daemon --features bench
```

Bench targets:

- Address/BIP32 scaling: `daemon/benches/address_scaling.rs`
- Address-filter scaling (`from_addresses`): `daemon/benches/from_addresses_filter.rs`
- UTXO refresh scaling: `daemon/benches/utxo_scaling.rs`
- UTXO snapshot reads under refresh (lock contention): `daemon/benches/utxo_contention.rs`

For extreme sizes (e.g. **1M addresses / 10M UTXOs**), use the dedicated stress bench binary (single-run timing, no Criterion sampling):

```bash
RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo run -p kaswallet-daemon --features bench --release --bin kaswallet-stress-bench -- \
  --i-understand --addresses 1000000 --utxos 10000000
```

## D3) What `kaswallet-stress-bench` measures (and what it doesn't)

This bench is meant to validate the **two reported pain points** (large address set + large UTXO set) in the hot in-memory refresh path, without involving RPC/network variability.

It measures (prints wall-clock time for):

1. Seeding **N synthetic wallet addresses** into `AddressManager` using bench-only insertion (no BIP32 derivation performed).
2. Building/warming the monitored-address caches:
   - `AddressManager::monitored_addresses()` (builds `Arc<Vec<Address>>`)
   - `AddressManager::monitored_address_map()` (builds/returns `Arc<HashMap<Address, WalletAddress>>`)
3. Generating **M synthetic UTXO entries** (`Vec<RpcUtxosByAddressesEntry>`).
4. Rebuilding the wallet UTXO set via `UtxoManager::update_utxo_set(...)` using the cached `Address -> WalletAddress` map (avoids cloning the full `AddressSet` for large wallets).
5. (Optional) **Contention proof**: with `--contend`, runs a second `update_utxo_set(...)` while sampling read latencies from multiple tasks:
   - `UtxoManager::state().await`
   - `UtxoManager::state_with_mempool().await`
   and prints p99/p999/max read latencies while the refresh is running.

It does **not** measure:

- RPC latency / node performance / gRPC serialization.
- Real BIP32 derivation cost (addresses are seeded synthetically).
- Disk/database performance (everything is in-memory).
- Reader/writer contention during refresh (use `daemon/benches/utxo_contention.rs` for concurrent snapshot reads while `update_utxo_set` runs).

## D4) Proving “refresh doesn’t block reads” at scale

Run the stress bench in contention mode:

```bash
RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo run -p kaswallet-daemon --features bench --release --bin kaswallet-stress-bench -- \
  --i-understand --addresses 1000000 --utxos 10000000 \
  --contend --contend-readers 8 --contend-sample-interval-micros 100
```

Interpretation:
- With the old `Arc<Mutex<UtxoManager>>` design, reads would commonly stall for **seconds** (≈ refresh duration).
- With the snapshot design, `state()` / `state_with_mempool()` should stay in the **µs** range; occasional small spikes are normal, but **ms→seconds** indicates lock contention or runtime starvation.

# F) Post-review refinements (2026-02-02)

After reviewing `PERF_OPTIMIZATIONS-REVIEW.md`, a few low-risk fixes were applied:

- `daemon/src/address_manager.rs`
  - Moved `address_set_version` increment **inside** the `addresses` mutex scope in `new_address(...)` and `change_address(...)` to remove the “inserted-but-old-version” window.
  - Avoided double `to_string()` by computing `address_string` once in `new_address(...)`.
- `daemon/src/service/get_balance.rs`
  - Added an early return for empty wallets (`utxos_count == 0`).
  - Avoided deriving address strings unless `include_balance_per_address` is `true`.
- `daemon/src/utxo_manager.rs`
  - Renamed typo variable `exculde` → `exclude`.
- `daemon/src/sync_manager.rs`
  - Fixed progress log typo: “addressed” → “addresses”.
- `common/src/proto_convert.rs`
  - Made `WalletUtxo::into_proto(self, ...)` actually consume `self` (no clone of outpoint/entry), matching naming expectations.

# E) Follow-ups (not implemented here)

- Replace periodic full `get_utxos_by_addresses` refresh with incremental updates/subscriptions if the node/RPC supports it.
- Add paging/limits to `GetUtxos` response for very large wallets (server-side).
- If address discovery continues to be a bottleneck, consider persistently tracking “ever-derived” ranges and/or using a tighter gap limit with explicit user-configurable policies.
