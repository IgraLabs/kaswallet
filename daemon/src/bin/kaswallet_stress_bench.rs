use clap::Parser;
use common::keys::Keys;
use common::model::{Keychain, WalletAddress, WalletSignableTransaction, WalletUtxoEntry};
use kaspa_addresses::{Address, Prefix as AddressPrefix, Version};
use kaspa_bip32::Prefix as XPubPrefix;
use kaspa_consensus_core::tx::TransactionId;
use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_consensus_core::tx::{
    SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput,
    UtxoEntry,
};
use kaspa_rpc_core::{RpcTransactionOutpoint, RpcUtxoEntry, RpcUtxosByAddressesEntry};
use kaswallet_daemon::address_manager::AddressManager;
use kaswallet_daemon::utxo_manager::UtxoManager;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tokio::time::MissedTickBehavior;

#[derive(Parser, Debug)]
#[command(about = "Synthetic stress benchmark for huge wallets (no RPC/network).")]
struct Args {
    /// Number of wallet addresses to seed into AddressManager.
    #[arg(long, default_value_t = 1_000_000)]
    addresses: u32,

    /// Number of UTXOs to generate and feed into UtxoManager::update_utxo_set.
    #[arg(long, default_value_t = 10_000_000)]
    utxos: u32,

    /// Progress log cadence (0 disables progress logs).
    #[arg(long, default_value_t = 100_000)]
    progress_every: u32,

    /// Required safety flag because this benchmark can use MANY GiB of RAM.
    #[arg(long)]
    i_understand: bool,

    /// Run a contention scenario: measure read latencies while a second `update_utxo_set` runs.
    #[arg(long)]
    contend: bool,

    /// Number of concurrent reader tasks when running `--contend`.
    #[arg(long, default_value_t = 4)]
    contend_readers: u32,

    /// Sampling interval (microseconds) for read latency measurements when running `--contend`.
    #[arg(long, default_value_t = 100)]
    contend_sample_interval_micros: u64,
}

fn address_for_index(prefix: AddressPrefix, i: u32) -> Address {
    let mut payload = [0u8; 32];
    payload[..4].copy_from_slice(&i.to_le_bytes());
    Address::new(prefix, Version::PubKey, &payload)
}

fn txid(i: u32) -> TransactionId {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&i.to_le_bytes());
    TransactionId::from_bytes(bytes)
}

fn summarize_latencies(name: &str, mut samples_ns: Vec<u64>) {
    if samples_ns.is_empty() {
        println!("  {name}: no samples");
        return;
    }
    samples_ns.sort_unstable();
    let n = samples_ns.len();
    let p99 = samples_ns[((n - 1) * 99) / 100];
    let p999 = samples_ns[((n - 1) * 999) / 1000];
    let max = *samples_ns.last().unwrap();

    let p99_us = (p99 as f64) / 1_000.0;
    let p999_us = (p999 as f64) / 1_000.0;
    let max_us = (max as f64) / 1_000.0;

    println!(
        "  {name}: samples={n} p99={p99_us:.3}µs p999={p999_us:.3}µs max={max_us:.3}µs"
    );
}

fn main() {
    let args = Args::parse();
    if !args.i_understand {
        eprintln!(
            "Refusing to run without --i-understand (this can use MANY GiB of RAM). \
Example:\n  RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo run -p kaswallet-daemon --features bench --release --bin kaswallet-stress-bench -- --i-understand --addresses 1000000 --utxos 10000000"
        );
        std::process::exit(2);
    }
    if args.addresses == 0 {
        eprintln!("--addresses must be > 0");
        std::process::exit(2);
    }

    let rt = Runtime::new().expect("tokio runtime");
    let prefix = AddressPrefix::Mainnet;

    // We don't derive addresses here; we seed AddressManager directly.
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

    let address_manager = AddressManager::new(keys, prefix);

    println!(
        "kaswallet-stress-bench: seeding addresses={} utxos={}",
        args.addresses, args.utxos
    );

    let start = Instant::now();
    rt.block_on(async {
        for i in 0..args.addresses {
            let address = address_for_index(prefix, i);
            let wa = WalletAddress::new(i, 0, Keychain::External);
            address_manager.insert_address_for_bench(address, wa).await;

            if args.progress_every > 0 && (i + 1) % args.progress_every == 0 {
                println!("  seeded {} addresses", i + 1);
            }
        }
    });
    println!("Seeded addresses in {:?}", start.elapsed());

    // Build and warm the monitored-address caches (both Vec<Address> and HashMap<Address, WalletAddress>).
    let start = Instant::now();
    let monitored = rt
        .block_on(address_manager.monitored_addresses())
        .expect("monitored_addresses");
    println!(
        "monitored_addresses first build: {:?} (len={})",
        start.elapsed(),
        monitored.len()
    );

    let start = Instant::now();
    let monitored2 = rt
        .block_on(address_manager.monitored_addresses())
        .expect("monitored_addresses cached");
    println!(
        "monitored_addresses cached: {:?} (same_arc={})",
        start.elapsed(),
        Arc::ptr_eq(&monitored, &monitored2)
    );

    let start = Instant::now();
    let by_address = rt
        .block_on(address_manager.monitored_address_map())
        .expect("monitored_address_map");
    println!(
        "monitored_address_map cached: {:?} (len={})",
        start.elapsed(),
        by_address.len()
    );

    let address_manager = Arc::new(Mutex::new(address_manager));
    let utxo_manager = Arc::new(UtxoManager::new_for_bench(address_manager));

    let empty_spk = ScriptPublicKey::from_vec(0, vec![]);

    println!("Generating {} UTXO entries...", args.utxos);
    let start = Instant::now();
    let mut entries: Vec<RpcUtxosByAddressesEntry> = Vec::with_capacity(args.utxos as usize);
    for i in 0..args.utxos {
        let address_index = i % args.addresses;
        let address = address_for_index(prefix, address_index);

        let outpoint = RpcTransactionOutpoint {
            transaction_id: txid(i),
            index: i,
        };
        let amount = ((i % 10_000) + 1) as u64;
        let utxo_entry = RpcUtxoEntry::new(amount, empty_spk.clone(), 0, false);

        entries.push(RpcUtxosByAddressesEntry {
            address: Some(address),
            outpoint,
            utxo_entry,
        });

        if args.progress_every > 0 && (i + 1) % args.progress_every == 0 {
            println!("  generated {} utxos", i + 1);
        }
    }
    println!("Generated UTXO entries in {:?}", start.elapsed());

    println!("Running update_utxo_set...");
    let start = Instant::now();
    rt.block_on(utxo_manager.update_utxo_set(entries, vec![]))
        .expect("update_utxo_set");
    let state = rt.block_on(utxo_manager.state());
    println!(
        "update_utxo_set: {:?} (utxos_by_outpoint={})",
        start.elapsed(),
        state.utxos_by_outpoint().len()
    );

    // Minimal sanity check to keep the compiler honest and confirm the sorted index exists.
    let mut sum = 0u64;
    for utxo in state.utxos_sorted_by_amount().take(1000) {
        sum = sum.wrapping_add(utxo.utxo_entry.amount);
    }
    println!("sanity: sum(first 1000 amounts)={sum}");

    if !args.contend {
        return;
    }

    if args.contend_readers == 0 {
        eprintln!("--contend-readers must be > 0");
        std::process::exit(2);
    }

    println!(
        "Starting contention run: readers={} sample_interval={}µs",
        args.contend_readers, args.contend_sample_interval_micros
    );

    let stop = Arc::new(AtomicBool::new(false));
    let utxo_manager_clone = Arc::clone(&utxo_manager);

    // Keep one wallet-local pending tx so `state_with_mempool()` includes the overlay path.
    rt.block_on(async {
        let input_outpoint = RpcTransactionOutpoint {
            transaction_id: txid(0),
            index: 0,
        };

        let input = TransactionInput::new(
            TransactionOutpoint::new(input_outpoint.transaction_id, input_outpoint.index),
            vec![],
            0,
            1,
        );
        let output = TransactionOutput::new(1, empty_spk.clone());
        let tx = Transaction::new(0, vec![input], vec![output], 0, Default::default(), 0, vec![]);

        let wallet_utxo_entry = WalletUtxoEntry::new(1, empty_spk.clone(), 0, false);
        let input_entry: UtxoEntry = wallet_utxo_entry.into();
        let signable = SignableTransaction::with_entries(tx, vec![input_entry]);

        let wa0 = WalletAddress::new(0, 0, Keychain::External);
        let a0 = address_for_index(prefix, 0);
        let wallet_tx = WalletSignableTransaction::new_from_unsigned(
            signable,
            HashSet::new(),
            vec![wa0],
            vec![a0],
        );
        utxo_manager_clone.add_mempool_transaction(&wallet_tx).await;
    });

    let contend_sample_interval = if args.contend_sample_interval_micros == 0 {
        None
    } else {
        Some(core::time::Duration::from_micros(
            args.contend_sample_interval_micros,
        ))
    };

    let utxo_manager_for_update = Arc::clone(&utxo_manager);
    let contend_address_count = args.addresses;
    let contend_utxo_count = args.utxos;
    let contend_prefix = prefix;

    let stop_clone = Arc::clone(&stop);
    let update_handle = rt.spawn(async move {
        println!("Contention: generating UTXOs for refresh...");
        let start = Instant::now();
        let empty_spk = ScriptPublicKey::from_vec(0, vec![]);
        let mut entries: Vec<RpcUtxosByAddressesEntry> =
            Vec::with_capacity(contend_utxo_count as usize);
        for i in 0..contend_utxo_count {
            let address_index = i % contend_address_count;
            let address = address_for_index(contend_prefix, address_index);
            let outpoint = RpcTransactionOutpoint {
                transaction_id: txid(i),
                index: i,
            };
            let amount = ((i % 10_000) + 1) as u64;
            let utxo_entry = RpcUtxoEntry::new(amount, empty_spk.clone(), 0, false);
            entries.push(RpcUtxosByAddressesEntry {
                address: Some(address),
                outpoint,
                utxo_entry,
            });
        }
        println!("Contention: generated UTXOs in {:?}", start.elapsed());

        println!("Contention: running update_utxo_set...");
        let start = Instant::now();
        utxo_manager_for_update
            .update_utxo_set(entries, vec![])
            .await
            .expect("contention update_utxo_set");
        let elapsed = start.elapsed();
        println!("Contention: update_utxo_set done in {elapsed:?}");

        stop_clone.store(true, Relaxed);
        elapsed
    });

    let mut reader_handles = Vec::new();
    for _ in 0..args.contend_readers {
        let utxo_manager = Arc::clone(&utxo_manager);
        let stop = Arc::clone(&stop);
        let sample_interval = contend_sample_interval;
        reader_handles.push(rt.spawn(async move {
            let mut state_samples_ns: Vec<u64> = Vec::new();
            let mut mempool_samples_ns: Vec<u64> = Vec::new();

            if let Some(interval_duration) = sample_interval {
                let mut interval = tokio::time::interval(interval_duration);
                interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    if stop.load(Relaxed) {
                        break;
                    }
                    let t0 = Instant::now();
                    let state = utxo_manager.state().await;
                    std::hint::black_box(state.utxo_count());
                    state_samples_ns.push(t0.elapsed().as_nanos() as u64);

                    let t0 = Instant::now();
                    let view = utxo_manager.state_with_mempool().await.unwrap();
                    std::hint::black_box(view.utxo_count());
                    mempool_samples_ns.push(t0.elapsed().as_nanos() as u64);
                }
            } else {
                while !stop.load(Relaxed) {
                    let t0 = Instant::now();
                    let state = utxo_manager.state().await;
                    std::hint::black_box(state.utxo_count());
                    state_samples_ns.push(t0.elapsed().as_nanos() as u64);

                    let t0 = Instant::now();
                    let view = utxo_manager.state_with_mempool().await.unwrap();
                    std::hint::black_box(view.utxo_count());
                    mempool_samples_ns.push(t0.elapsed().as_nanos() as u64);
                }
            }

            (state_samples_ns, mempool_samples_ns)
        }));
    }

    let update_elapsed = rt
        .block_on(async { update_handle.await.expect("update task panicked") });
    println!("Contention: update_utxo_set elapsed = {update_elapsed:?}");

    // Ensure readers stop even if the update task completed before they started.
    stop.store(true, Relaxed);

    let mut merged_state_ns = Vec::new();
    let mut merged_mempool_ns = Vec::new();
    for handle in reader_handles {
        let (state_ns, mempool_ns) = rt
            .block_on(async { handle.await.expect("reader task panicked") });
        merged_state_ns.extend(state_ns);
        merged_mempool_ns.extend(mempool_ns);
    }

    println!("Read latency while update_utxo_set was running:");
    summarize_latencies("state().await + utxo_count", merged_state_ns);
    summarize_latencies(
        "state_with_mempool().await + utxo_count",
        merged_mempool_ns,
    );
}
