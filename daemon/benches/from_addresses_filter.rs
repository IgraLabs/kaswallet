use common::model::{Keychain, WalletAddress};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::collections::HashSet;
use std::time::Duration;

fn bench_from_addresses_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("from_addresses_filter");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));

    let address_pool_size: u32 = 10_000;
    let address_pool: Vec<WalletAddress> = (0..address_pool_size)
        .map(|i| WalletAddress::new(i, 0, Keychain::External))
        .collect();

    let utxo_counts: [usize; 2] = [200_000, 1_000_000];
    let filter_lens: [usize; 4] = [0, 1, 10, 100];

    for &utxo_count in &utxo_counts {
        let utxo_addresses: Vec<WalletAddress> = (0..utxo_count as u32)
            .map(|i| WalletAddress::new(i % address_pool_size, 0, Keychain::External))
            .collect();

        for &filter_len in &filter_lens {
            let from_addresses: Vec<&WalletAddress> = address_pool.iter().take(filter_len).collect();

            group.bench_with_input(
                BenchmarkId::new(format!("linear/utxos_{utxo_count}"), filter_len),
                &filter_len,
                |b, _| {
                    b.iter(|| {
                        let mut kept = 0usize;
                        for wa in &utxo_addresses {
                            if !from_addresses.is_empty() && !from_addresses.contains(&&wa) {
                                continue;
                            }
                            kept += 1;
                        }
                        black_box(kept);
                    })
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("hashset/utxos_{utxo_count}"), filter_len),
                &filter_len,
                |b, _| {
                    b.iter(|| {
                        let from_set: Option<HashSet<WalletAddress>> = if from_addresses.is_empty() {
                            None
                        } else {
                            Some(from_addresses.iter().map(|wa| (*wa).clone()).collect())
                        };

                        let mut kept = 0usize;
                        for wa in &utxo_addresses {
                            if let Some(ref set) = from_set {
                                if !set.contains(wa) {
                                    continue;
                                }
                            }
                            kept += 1;
                        }
                        black_box(kept);
                    })
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_from_addresses_filter);
criterion_main!(benches);

