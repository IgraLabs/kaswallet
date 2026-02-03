use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use common::addresses::{multisig_address, multisig_address_from_sorted_keys};
use common::keys::{master_key_path, Keys};
use common::model::{Keychain, WalletAddress};
use kaspa_addresses::{Address, Prefix as AddressPrefix, Version};
use kaspa_bip32::secp256k1::SecretKey;
use kaspa_bip32::{DerivationPath, ExtendedPrivateKey, ExtendedPublicKey, Language, Mnemonic, Prefix as XPubPrefix};
use kaswallet_daemon::address_manager::AddressManager;
use std::sync::Arc;
use tokio::runtime::Runtime;

fn xpub_from_mnemonic(
    phrase: &str,
    is_multisig: bool,
) -> ExtendedPublicKey<kaspa_bip32::secp256k1::PublicKey> {
    let mnemonic = Mnemonic::new(phrase, Language::English).unwrap();
    let seed = mnemonic.to_seed("");
    let xprv = ExtendedPrivateKey::<SecretKey>::new(seed).unwrap();
    let xprv = xprv.derive_path(&master_key_path(is_multisig)).unwrap();
    xprv.public_key()
}

fn make_keys(
    public_keys: Vec<ExtendedPublicKey<kaspa_bip32::secp256k1::PublicKey>>,
    minimum_signatures: u16,
) -> Arc<Keys> {
    Arc::new(Keys::new(
        "bench-unused-keys.json".to_string(),
        1,
        vec![],
        XPubPrefix::XPUB,
        public_keys,
        0,
        0,
        minimum_signatures,
        0,
    ))
}

fn baseline_calculate_address_path(wallet_address: &WalletAddress, is_multisig: bool) -> DerivationPath {
    let keychain_number = wallet_address.keychain.clone() as u32;
    let path_string = if is_multisig {
        format!(
            "m/{}/{}/{}",
            wallet_address.cosigner_index, keychain_number, wallet_address.index
        )
    } else {
        format!("m/{}/{}", keychain_number, wallet_address.index)
    };
    path_string.parse().unwrap()
}

fn bench_calculate_address_path(c: &mut Criterion) {
    let keys = make_keys(
        vec![xpub_from_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            false,
        )],
        1,
    );
    let manager = AddressManager::new(keys, AddressPrefix::Mainnet);

    let wallet_addresses: Vec<WalletAddress> = (0..256)
        .map(|i| WalletAddress::new(i, 0, Keychain::External))
        .collect();

    c.bench_function("calculate_address_path/new (singlesig)", |b| {
        b.iter(|| {
            for wa in &wallet_addresses {
                black_box(manager.calculate_address_path(wa).unwrap());
            }
        })
    });

    c.bench_function("calculate_address_path/baseline parse (singlesig)", |b| {
        b.iter(|| {
            for wa in &wallet_addresses {
                black_box(baseline_calculate_address_path(wa, false));
            }
        })
    });
}

fn bench_multisig_sorted_vs_unsorted(c: &mut Criterion) {
    let xpub1 = xpub_from_mnemonic(
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        true,
    );
    let xpub2 = xpub_from_mnemonic(
        "legal winner thank year wave sausage worth useful legal winner thank yellow",
        true,
    );
    let xpub3 = xpub_from_mnemonic(
        "letter advice cage absurd amount doctor acoustic avoid letter advice cage above",
        true,
    );

    let unsorted = Arc::new(vec![xpub2.clone(), xpub3.clone(), xpub1.clone()]);
    let mut sorted = vec![xpub2, xpub3, xpub1];
    sorted.sort();

    let derivation_path: DerivationPath = "m/0/0/0".parse().unwrap();

    c.bench_function("multisig_address (sorts each call)", |b| {
        b.iter(|| {
            black_box(
                multisig_address(
                    unsorted.clone(),
                    2,
                    AddressPrefix::Devnet,
                    &derivation_path,
                )
                .unwrap(),
            );
        })
    });

    c.bench_function("multisig_address_from_sorted_keys (no per-call sort)", |b| {
        b.iter(|| {
            black_box(
                multisig_address_from_sorted_keys(
                    &sorted,
                    2,
                    AddressPrefix::Devnet,
                    &derivation_path,
                )
                .unwrap(),
            );
        })
    });
}

fn bench_monitored_addresses_cache(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let keys = make_keys(
        vec![xpub_from_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            false,
        )],
        1,
    );
    let manager = AddressManager::new(keys, AddressPrefix::Mainnet);

    // Seed a large monitored set without disk IO (bench-only helpers).
    let address_count: u32 = 10_000;
    rt.block_on(async {
        for i in 0..address_count {
            let mut payload = [0u8; 32];
            payload[..4].copy_from_slice(&i.to_le_bytes());
            let address = Address::new(AddressPrefix::Mainnet, Version::PubKey, &payload);
            let wa = WalletAddress::new(i, 0, Keychain::External);
            manager.insert_address_for_bench(address, wa).await;
        }
        // Warm cache once.
        manager.monitored_addresses().await.unwrap();
    });

    c.bench_function("monitored_addresses/cached (10k)", |b| {
        b.iter(|| {
            let addresses = rt.block_on(manager.monitored_addresses()).unwrap();
            black_box(addresses.len());
        })
    });

    c.bench_function("monitored_addresses/rebuild (10k)", |b| {
        b.iter(|| {
            manager.bump_address_set_version_for_bench();
            let addresses = rt.block_on(manager.monitored_addresses()).unwrap();
            black_box(addresses.len());
        })
    });
}

fn bench_addresses_to_query(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let single_keys = make_keys(
        vec![xpub_from_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            false,
        )],
        1,
    );
    let single = AddressManager::new(single_keys, AddressPrefix::Mainnet);

    let multi_keys = make_keys(
        vec![
            xpub_from_mnemonic(
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                true,
            ),
            xpub_from_mnemonic(
                "legal winner thank year wave sausage worth useful legal winner thank yellow",
                true,
            ),
            xpub_from_mnemonic(
                "letter advice cage absurd amount doctor acoustic avoid letter advice cage above",
                true,
            ),
        ],
        2,
    );
    let multi = AddressManager::new(multi_keys, AddressPrefix::Mainnet);

    let mut group = c.benchmark_group("addresses_to_query");
    for &indexes in &[10u32, 100, 1_000] {
        group.bench_with_input(BenchmarkId::new("singlesig", indexes), &indexes, |b, &indexes| {
            b.iter(|| {
                let set = rt
                    .block_on(single.addresses_to_query(0, indexes))
                    .unwrap();
                black_box(set.len());
            })
        });

        // Multisig derivation is significantly more expensive; keep the upper bound smaller.
        if indexes <= 100 {
            group.bench_with_input(
                BenchmarkId::new("multisig (2-of-3)", indexes),
                &indexes,
                |b, &indexes| {
                    b.iter(|| {
                        let set = rt
                            .block_on(multi.addresses_to_query(0, indexes))
                            .unwrap();
                        black_box(set.len());
                    })
                },
            );
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_calculate_address_path,
    bench_multisig_sorted_vs_unsorted,
    bench_monitored_addresses_cache,
    bench_addresses_to_query
);
criterion_main!(benches);

