use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use common::keys::Keys;
use common::model::{Keychain, WalletAddress, WalletSignableTransaction, WalletUtxoEntry};
use kaspa_addresses::{Address, Prefix as AddressPrefix, Version};
use kaspa_bip32::Prefix as XPubPrefix;
use kaspa_consensus_core::tx::{
    ScriptPublicKey, SignableTransaction, Transaction, TransactionInput, TransactionOutpoint,
    TransactionOutput, UtxoEntry,
};
use kaspa_hashes::Hash;
use kaspa_rpc_core::{RpcTransactionOutpoint, RpcUtxoEntry, RpcUtxosByAddressesEntry};
use kaswallet_daemon::address_manager::AddressManager;
use kaswallet_daemon::utxo_manager::UtxoManager;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

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

fn bench_utxo_state_reads_while_updating(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

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
    let address_count: u32 = 10_000;

    // Seed address manager with a large set of monitored addresses.
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

    let address_manager = Arc::new(Mutex::new(address_manager));
    rt.block_on(async {
        let guard = address_manager.lock().await;
        guard.monitored_address_map().await.unwrap();
    });

    let utxo_manager = Arc::new(UtxoManager::new_for_bench(address_manager.clone()));

    let utxo_count: u32 = 10_000;
    let base_entries: Vec<RpcUtxosByAddressesEntry> = (0..utxo_count)
        .map(|i| make_rpc_utxo(i, addresses[(i % address_count) as usize].clone()))
        .collect();

    // Establish initial state and keep one wallet-local pending tx in the overlay.
    rt.block_on(async {
        utxo_manager
            .update_utxo_set(base_entries.clone(), vec![])
            .await
            .unwrap();

        // Spend one known outpoint and create one output to our address (wallet-local mempool overlay).
        let input_outpoint = make_outpoint(0);
        let wallet_utxo_entry: WalletUtxoEntry = make_rpc_utxo_entry(1).into();
        let input_entry: UtxoEntry = wallet_utxo_entry.clone().into();

        let input = TransactionInput::new(
            TransactionOutpoint::new(input_outpoint.transaction_id, input_outpoint.index),
            vec![],
            0,
            1,
        );
        let output = TransactionOutput::new(1, ScriptPublicKey::from_vec(0, vec![]));
        let tx = Transaction::new(0, vec![input], vec![output], 0, Default::default(), 0, vec![]);
        let signable = SignableTransaction::with_entries(tx, vec![input_entry]);

        let wa0 = WalletAddress::new(0, 0, Keychain::External);
        let a0 = addresses[0].clone();
        let wallet_tx: WalletSignableTransaction =
            WalletSignableTransaction::new_from_unsigned(signable, HashSet::new(), vec![wa0], vec![a0]);
        utxo_manager.add_mempool_transaction(&wallet_tx).await;
    });

    // Background refresh loop to exercise write-lock swaps while measuring reads.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);
    let utxo_manager_clone = Arc::clone(&utxo_manager);
    let refresh_entries = base_entries.clone();
    let refresh_task = rt.spawn(async move {
        while !stop_clone.load(Relaxed) {
            utxo_manager_clone
                .update_utxo_set(refresh_entries.clone(), vec![])
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    let mut group = c.benchmark_group("utxo_contention");
    group.bench_function(BenchmarkId::new("state", "utxo_count"), |b| {
        b.iter(|| {
            let count = rt.block_on(async { utxo_manager.state().await.utxo_count() });
            black_box(count);
        });
    });
    group.bench_function(BenchmarkId::new("state", "sorted_take_10_sum"), |b| {
        b.iter(|| {
            let sum = rt.block_on(async {
                let state = utxo_manager.state().await;
                state
                    .utxos_sorted_by_amount()
                    .take(10)
                    .map(|utxo| utxo.utxo_entry.amount)
                    .sum::<u64>()
            });
            black_box(sum);
        });
    });
    group.bench_function(BenchmarkId::new("state_with_mempool", "utxo_count"), |b| {
        b.iter(|| {
            let count = rt.block_on(async {
                let view = utxo_manager.state_with_mempool().await.unwrap();
                view.utxo_count()
            });
            black_box(count);
        });
    });
    group.finish();

    stop.store(true, Relaxed);
    refresh_task.abort();
    let _ = rt.block_on(refresh_task);
}

criterion_group!(benches, bench_utxo_state_reads_while_updating);
criterion_main!(benches);

