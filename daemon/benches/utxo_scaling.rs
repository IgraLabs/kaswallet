use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use common::keys::Keys;
use common::model::{Keychain, WalletAddress};
use kaspa_addresses::Prefix as AddressPrefix;
use kaspa_bip32::Prefix as XPubPrefix;
use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_hashes::Hash;
use kaspa_rpc_core::{RpcTransactionOutpoint, RpcUtxoEntry, RpcUtxosByAddressesEntry};
use kaswallet_daemon::address_manager::AddressManager;
use kaswallet_daemon::utxo_manager::UtxoManager;
use std::sync::Arc;
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

fn make_rpc_utxo(i: u32, address: kaspa_addresses::Address) -> RpcUtxosByAddressesEntry {
    let amount = ((i % 10_000) + 1) as u64;
    RpcUtxosByAddressesEntry {
        address: Some(address),
        outpoint: make_outpoint(i),
        utxo_entry: make_rpc_utxo_entry(amount),
    }
}

fn bench_update_utxo_set(c: &mut Criterion) {
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

    // Seed address_manager with a realistic number of monitored addresses.
    let address_manager = AddressManager::new(keys, AddressPrefix::Mainnet);
    let address_count: u32 = 20_000;
    let mut addresses = Vec::with_capacity(address_count as usize);
    rt.block_on(async {
        for i in 0..address_count {
            let mut payload = [0u8; 32];
            payload[..4].copy_from_slice(&i.to_le_bytes());
            let address = kaspa_addresses::Address::new(AddressPrefix::Mainnet, kaspa_addresses::Version::PubKey, &payload);
            let wa = WalletAddress::new(i, 0, Keychain::External);
            address_manager.insert_address_for_bench(address.clone(), wa).await;
            addresses.push(address);
        }
    });

    let address_manager = Arc::new(Mutex::new(address_manager));
    let mut utxo_manager = UtxoManager::new_for_bench(address_manager);

    let mut group = c.benchmark_group("update_utxo_set");
    for &utxo_count in &[1_000u32, 10_000, 50_000] {
        let base_entries: Vec<RpcUtxosByAddressesEntry> = (0..utxo_count)
            .map(|i| make_rpc_utxo(i, addresses[(i % address_count) as usize].clone()))
            .collect();

        group.bench_with_input(BenchmarkId::from_parameter(utxo_count), &utxo_count, |b, _| {
            b.iter(|| {
                let entries = base_entries.clone();
                rt.block_on(utxo_manager.update_utxo_set(entries, vec![]))
                    .unwrap();
                black_box(utxo_manager.utxos_by_outpoint().len());
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_update_utxo_set);
criterion_main!(benches);
