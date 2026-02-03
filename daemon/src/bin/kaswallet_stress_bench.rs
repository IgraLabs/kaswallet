use clap::Parser;
use common::keys::Keys;
use common::model::{Keychain, WalletAddress};
use kaspa_addresses::{Address, Prefix as AddressPrefix, Version};
use kaspa_bip32::Prefix as XPubPrefix;
use kaspa_consensus_core::tx::TransactionId;
use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_rpc_core::{RpcTransactionOutpoint, RpcUtxoEntry, RpcUtxosByAddressesEntry};
use kaswallet_daemon::address_manager::AddressManager;
use kaswallet_daemon::utxo_manager::UtxoManager;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

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
    let mut utxo_manager = UtxoManager::new_for_bench(address_manager);

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
    println!(
        "update_utxo_set: {:?} (utxos_by_outpoint={})",
        start.elapsed(),
        utxo_manager.utxos_by_outpoint().len()
    );

    // Minimal sanity check to keep the compiler honest and confirm the sorted index exists.
    let mut sum = 0u64;
    for utxo in utxo_manager.utxos_sorted_by_amount().take(1000) {
        sum = sum.wrapping_add(utxo.utxo_entry.amount);
    }
    println!("sanity: sum(first 1000 amounts)={sum}");
}
