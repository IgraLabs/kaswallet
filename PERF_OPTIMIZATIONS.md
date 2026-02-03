# Performance optimizations (2026-02-02)

This document describes the code changes made to address **two long-running performance issues**:

1) **Slowdowns as the wallet UTXO set grows large** (CPU + memory + lock contention).
2) **Slowdowns as the wallet accumulates many derived addresses (BIP32)**.

The implemented address optimizations correspond to the **first 4 points** from the BIP32/address perf discussion:

1. Stop rescanning from index `0` every sync tick.
2. Eliminate string round-trips in discovery/balance queries.
3. Make address derivation cheaper (no path string parsing; avoid per-derivation multisig key sorting).
4. Cache monitored addresses so refresh does not rebuild/parse each tick.

## Files touched

- Updated:
  - `client/src/client.rs`
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

---

# A) UTXO set scaling

## A1. UTXO storage/indexing (`daemon/src/utxo_manager.rs`)

### Summary

- Keep UTXOs in a single map:
  - `utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>`
- Maintain a *lightweight* sorted index by amount:
  - `utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>` sorted by `(amount, outpoint)`
- Expose sorted iteration without cloning:
  - `UtxoManager::utxos_sorted_by_amount(&self) -> impl Iterator<Item = &WalletUtxo>`

### Why it helps

- Avoids storing the full `WalletUtxo` twice (once in a `HashMap`, once in a sorted `Vec`).
- Avoids an **O(n)** scan to locate an item to remove from the sorted list.
  - Removal now uses a `binary_search` on `(amount, outpoint)` to find the position.

### Key code locations

- `UtxoManager::insert_utxo` and `UtxoManager::remove_utxo`
- `UtxoManager::utxos_sorted_by_amount`
- `UtxoManager::update_utxo_set` (rebuilds map + sorted key index)

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

- `SyncManager::refresh_utxos` no longer holds the global `utxo_manager` mutex while doing RPC calls.
- The mutex is held only for the actual `UtxoManager::update_utxo_set(...)` mutation.

### Key code locations

- `daemon/src/sync_manager.rs` `refresh_utxos(...)`

## A4. Transaction selection paths (`daemon/src/transaction_generator.rs`)

### Summary

- Avoid cloning the full UTXO set inside:
  - `select_utxos(...)`
  - `more_utxos_for_merge_transaction(...)`
- Avoid hashing full `WalletUtxo` values; use `WalletOutpoint` identity instead.

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
- Cache invalidation occurs on:
  - `new_address(...)`
  - `change_address(...)`
  - `update_addresses_and_last_used_indexes(...)` (when it inserts any new addresses)

`SyncManager::refresh_utxos(...)` now uses this cached list and includes an early return for an empty wallet (avoid unnecessary RPC).

### Key code locations

- `daemon/src/address_manager.rs`
  - `AddressManager::monitored_addresses(...)`
  - `address_set_version` increments on address set changes
- `daemon/src/sync_manager.rs`
  - `refresh_utxos(...)` uses `monitored_addresses()`

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
  - `insert_remove_keeps_sorted_keys_consistent`

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
- UTXO refresh scaling: `daemon/benches/utxo_scaling.rs`

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
