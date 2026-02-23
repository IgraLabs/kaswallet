//! Comparative benchmarks: old `Mutex<UtxoManager>` pattern vs new `RwLock<Arc<UtxoState>>` pattern.
//!
//! The "old" code path is simulated here to avoid depending on deleted code.
//! It mirrors what origin/main did: a `Mutex` wrapping a `Vec<WalletUtxo>` + `HashMap`,
//! with `.clone()` on every read and linear `find_position` for removal.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use common::keys::Keys;
use common::model::{Keychain, WalletAddress, WalletOutpoint, WalletUtxo, WalletUtxoEntry};
use kaspa_addresses::{Address, Prefix as AddressPrefix, Version};
use kaspa_bip32::Prefix as XPubPrefix;
use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_hashes::Hash;
use kaspa_rpc_core::{RpcTransactionOutpoint, RpcUtxoEntry, RpcUtxosByAddressesEntry};
use kaswallet_daemon::address_manager::AddressManager;
use kaswallet_daemon::utxo_manager::UtxoManager;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Helpers (shared)
// ---------------------------------------------------------------------------

fn txid(i: u32) -> Hash {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&i.to_le_bytes());
    Hash::from_bytes(bytes)
}

fn make_outpoint(i: u32) -> RpcTransactionOutpoint {
    RpcTransactionOutpoint {
        transaction_id: txid(i),
        index: i,
    }
}

fn make_rpc_utxo_entry(amount: u64) -> RpcUtxoEntry {
    RpcUtxoEntry::new(amount, ScriptPublicKey::from_vec(0, vec![]), 0, false)
}

fn make_address(prefix: AddressPrefix, i: u32) -> Address {
    let mut payload = [0u8; 32];
    payload[..4].copy_from_slice(&i.to_le_bytes());
    Address::new(prefix, Version::PubKey, &payload)
}

fn make_rpc_utxo(i: u32, address: Address) -> RpcUtxosByAddressesEntry {
    let amount = ((i % 10_000) + 1) as u64;
    RpcUtxosByAddressesEntry {
        address: Some(address),
        outpoint: make_outpoint(i),
        utxo_entry: make_rpc_utxo_entry(amount),
    }
}

fn make_wallet_utxo(i: u32) -> WalletUtxo {
    let amount = ((i % 10_000) + 1) as u64;
    let outpoint = WalletOutpoint::new(txid(i), i);
    let entry = WalletUtxoEntry::new(amount, ScriptPublicKey::from_vec(0, vec![]), 0, false);
    let wa = WalletAddress::new(i % 1000, 0, Keychain::External);
    WalletUtxo::new(outpoint, entry, wa)
}

// ---------------------------------------------------------------------------
// Old-style simulation: Mutex<OldUtxoState>
// ---------------------------------------------------------------------------

struct OldUtxoState {
    utxos_sorted_by_amount: Vec<WalletUtxo>,
    utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,
}

impl OldUtxoState {
    fn from_utxos(utxos: Vec<WalletUtxo>) -> Self {
        let mut utxos_sorted = utxos.clone();
        utxos_sorted.sort_by(|a, b| a.utxo_entry.amount.cmp(&b.utxo_entry.amount));
        let utxos_by_outpoint: HashMap<_, _> = utxos
            .into_iter()
            .map(|u| (u.outpoint.clone(), u))
            .collect();
        Self {
            utxos_sorted_by_amount: utxos_sorted,
            utxos_by_outpoint,
        }
    }

    // Old API: clone entire Vec on read.
    fn utxos_sorted_by_amount(&self) -> Vec<WalletUtxo> {
        self.utxos_sorted_by_amount.clone()
    }

    // Old API: clone entire HashMap on read.
    fn utxos_by_outpoint(&self) -> HashMap<WalletOutpoint, WalletUtxo> {
        self.utxos_by_outpoint.clone()
    }

    fn utxo_count(&self) -> usize {
        self.utxos_by_outpoint.len()
    }

    // Old API: rebuild entire state (simulates update_utxo_set).
    fn replace_all(&mut self, utxos: Vec<WalletUtxo>) {
        let mut sorted = utxos.clone();
        sorted.sort_by(|a, b| a.utxo_entry.amount.cmp(&b.utxo_entry.amount));
        self.utxos_sorted_by_amount = sorted;
        self.utxos_by_outpoint.clear();
        for utxo in utxos {
            self.utxos_by_outpoint
                .insert(utxo.outpoint.clone(), utxo);
        }
    }
}

// ---------------------------------------------------------------------------
// Setup helpers
// ---------------------------------------------------------------------------

fn setup_address_manager(rt: &Runtime, address_count: u32) -> (Arc<Mutex<AddressManager>>, Vec<Address>) {
    let keys = Arc::new(Keys::new(
        "bench-unused-keys.json".to_string(),
        1,
        vec![],
        XPubPrefix::XPUB,
        vec![],
        0,
        0,
        1,
        0,
    ));

    let prefix = AddressPrefix::Mainnet;
    let address_manager = AddressManager::new(keys, prefix);
    let mut addresses = Vec::with_capacity(address_count as usize);
    rt.block_on(async {
        for i in 0..address_count {
            let address = make_address(prefix, i);
            let wa = WalletAddress::new(i, 0, Keychain::External);
            address_manager
                .insert_address_for_bench(address.clone(), wa)
                .await;
            addresses.push(address);
        }
    });

    let am = Arc::new(Mutex::new(address_manager));
    rt.block_on(async {
        let guard = am.lock().await;
        guard.monitored_address_map().await.unwrap();
    });

    (am, addresses)
}

// ---------------------------------------------------------------------------
// Benchmark 1: Snapshot read latency (utxo_count + sorted iteration)
//
// Old: lock Mutex, clone Vec, unlock, iterate
// New: clone Arc (ref-count bump), iterate through reference
// ---------------------------------------------------------------------------

fn bench_snapshot_read(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let utxo_count: u32 = 10_000;
    let address_count: u32 = 1_000;

    let (am, addresses) = setup_address_manager(&rt, address_count);

    // Build shared UTXO data.
    let wallet_utxos: Vec<WalletUtxo> = (0..utxo_count).map(make_wallet_utxo).collect();
    let rpc_entries: Vec<RpcUtxosByAddressesEntry> = (0..utxo_count)
        .map(|i| make_rpc_utxo(i, addresses[(i % address_count) as usize].clone()))
        .collect();

    // Old: Mutex<OldUtxoState>
    let old_state = Arc::new(tokio::sync::Mutex::new(OldUtxoState::from_utxos(wallet_utxos)));

    // New: UtxoManager with RwLock<Arc<UtxoState>>
    let new_mgr = Arc::new(UtxoManager::new_for_bench(am));
    rt.block_on(async {
        new_mgr
            .update_utxo_set(rpc_entries, vec![])
            .await
            .unwrap();
    });

    let mut group = c.benchmark_group("snapshot_read");

    // -- utxo_count --

    group.bench_function(BenchmarkId::new("old_mutex_clone", "utxo_count"), |b| {
        b.iter(|| {
            let count = rt.block_on(async {
                let guard = old_state.lock().await;
                guard.utxo_count()
            });
            black_box(count);
        });
    });

    group.bench_function(BenchmarkId::new("new_rwlock_arc", "utxo_count"), |b| {
        b.iter(|| {
            let count = rt.block_on(async { new_mgr.state().await.utxo_count() });
            black_box(count);
        });
    });

    // -- sorted take(10) sum --

    group.bench_function(BenchmarkId::new("old_mutex_clone", "sorted_take_10"), |b| {
        b.iter(|| {
            let sum = rt.block_on(async {
                let guard = old_state.lock().await;
                let sorted = guard.utxos_sorted_by_amount(); // full clone
                sorted
                    .iter()
                    .take(10)
                    .map(|u| u.utxo_entry.amount)
                    .sum::<u64>()
            });
            black_box(sum);
        });
    });

    group.bench_function(BenchmarkId::new("new_rwlock_arc", "sorted_take_10"), |b| {
        b.iter(|| {
            let sum = rt.block_on(async {
                let state = new_mgr.state().await;
                state
                    .utxos_sorted_by_amount()
                    .take(10)
                    .map(|u| u.utxo_entry.amount)
                    .sum::<u64>()
            });
            black_box(sum);
        });
    });

    // -- full sorted iteration sum --

    group.bench_function(BenchmarkId::new("old_mutex_clone", "sorted_full_sum"), |b| {
        b.iter(|| {
            let sum = rt.block_on(async {
                let guard = old_state.lock().await;
                let sorted = guard.utxos_sorted_by_amount(); // full clone
                sorted.iter().map(|u| u.utxo_entry.amount).sum::<u64>()
            });
            black_box(sum);
        });
    });

    group.bench_function(BenchmarkId::new("new_rwlock_arc", "sorted_full_sum"), |b| {
        b.iter(|| {
            let sum = rt.block_on(async {
                let state = new_mgr.state().await;
                state
                    .utxos_sorted_by_amount()
                    .map(|u| u.utxo_entry.amount)
                    .sum::<u64>()
            });
            black_box(sum);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 2: Read throughput under write contention
//
// A background task continuously refreshes the UTXO set (simulating periodic sync).
// The benchmark measures how fast readers can get the sorted UTXO snapshot.
//
// Old: Mutex means readers block on writers.
// New: RwLock<Arc> means readers grab a snapshot pointer instantly.
// ---------------------------------------------------------------------------

fn bench_contention(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let utxo_count: u32 = 10_000;
    let address_count: u32 = 1_000;

    let (am, addresses) = setup_address_manager(&rt, address_count);

    let wallet_utxos: Vec<WalletUtxo> = (0..utxo_count).map(make_wallet_utxo).collect();
    let rpc_entries: Vec<RpcUtxosByAddressesEntry> = (0..utxo_count)
        .map(|i| make_rpc_utxo(i, addresses[(i % address_count) as usize].clone()))
        .collect();

    // -- Old pattern: Mutex<OldUtxoState> with background writer --
    let old_state = Arc::new(tokio::sync::Mutex::new(OldUtxoState::from_utxos(
        wallet_utxos.clone(),
    )));

    let stop_old = Arc::new(AtomicBool::new(false));
    let old_writer = {
        let old_state = Arc::clone(&old_state);
        let stop = Arc::clone(&stop_old);
        let utxos = wallet_utxos.clone();
        rt.spawn(async move {
            while !stop.load(Relaxed) {
                {
                    let mut guard = old_state.lock().await;
                    guard.replace_all(utxos.clone());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };

    // -- New pattern: UtxoManager with background writer --
    let new_mgr = Arc::new(UtxoManager::new_for_bench(am));
    rt.block_on(async {
        new_mgr
            .update_utxo_set(rpc_entries.clone(), vec![])
            .await
            .unwrap();
    });

    let stop_new = Arc::new(AtomicBool::new(false));
    let new_writer = {
        let new_mgr = Arc::clone(&new_mgr);
        let stop = Arc::clone(&stop_new);
        let entries = rpc_entries;
        rt.spawn(async move {
            while !stop.load(Relaxed) {
                new_mgr
                    .update_utxo_set(entries.clone(), vec![])
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };

    let mut group = c.benchmark_group("contention");

    group.bench_function(BenchmarkId::new("old_mutex", "sorted_take_10"), |b| {
        b.iter(|| {
            let sum = rt.block_on(async {
                let guard = old_state.lock().await;
                let sorted = guard.utxos_sorted_by_amount();
                sorted
                    .iter()
                    .take(10)
                    .map(|u| u.utxo_entry.amount)
                    .sum::<u64>()
            });
            black_box(sum);
        });
    });

    group.bench_function(BenchmarkId::new("new_rwlock_arc", "sorted_take_10"), |b| {
        b.iter(|| {
            let sum = rt.block_on(async {
                let state = new_mgr.state().await;
                state
                    .utxos_sorted_by_amount()
                    .take(10)
                    .map(|u| u.utxo_entry.amount)
                    .sum::<u64>()
            });
            black_box(sum);
        });
    });

    group.bench_function(BenchmarkId::new("old_mutex", "utxo_count"), |b| {
        b.iter(|| {
            let count = rt.block_on(async {
                let guard = old_state.lock().await;
                guard.utxo_count()
            });
            black_box(count);
        });
    });

    group.bench_function(BenchmarkId::new("new_rwlock_arc", "utxo_count"), |b| {
        b.iter(|| {
            let count = rt.block_on(async { new_mgr.state().await.utxo_count() });
            black_box(count);
        });
    });

    group.finish();

    stop_old.store(true, Relaxed);
    stop_new.store(true, Relaxed);
    old_writer.abort();
    new_writer.abort();
    let _ = rt.block_on(old_writer);
    let _ = rt.block_on(new_writer);
}

// ---------------------------------------------------------------------------
// Benchmark 3: HashMap clone cost (old) vs reference access (new)
//
// Old code returned utxos_by_outpoint().clone() on every read.
// New code gives direct reference access through the Arc snapshot.
// ---------------------------------------------------------------------------

fn bench_outpoint_lookup(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let utxo_count: u32 = 10_000;
    let address_count: u32 = 1_000;

    let (am, addresses) = setup_address_manager(&rt, address_count);

    let wallet_utxos: Vec<WalletUtxo> = (0..utxo_count).map(make_wallet_utxo).collect();
    let rpc_entries: Vec<RpcUtxosByAddressesEntry> = (0..utxo_count)
        .map(|i| make_rpc_utxo(i, addresses[(i % address_count) as usize].clone()))
        .collect();

    // Target outpoint to look up (middle of the set).
    let target = WalletOutpoint::new(txid(utxo_count / 2), utxo_count / 2);

    let old_state = Arc::new(tokio::sync::Mutex::new(OldUtxoState::from_utxos(wallet_utxos)));

    let new_mgr = Arc::new(UtxoManager::new_for_bench(am));
    rt.block_on(async {
        new_mgr
            .update_utxo_set(rpc_entries, vec![])
            .await
            .unwrap();
    });

    let mut group = c.benchmark_group("outpoint_lookup");

    group.bench_function(BenchmarkId::new("old_clone_hashmap", "lookup_one"), |b| {
        b.iter(|| {
            let found = rt.block_on(async {
                let guard = old_state.lock().await;
                let map = guard.utxos_by_outpoint(); // full HashMap clone
                map.get(&target).is_some()
            });
            black_box(found);
        });
    });

    group.bench_function(BenchmarkId::new("new_arc_ref", "lookup_one"), |b| {
        b.iter(|| {
            let found = rt.block_on(async {
                let state = new_mgr.state().await;
                state.get_utxo_by_outpoint(&target).is_some()
            });
            black_box(found);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_snapshot_read, bench_contention, bench_outpoint_lookup);
criterion_main!(benches);
