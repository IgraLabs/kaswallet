use clap::Parser;
use kaspa_consensus_core::network::NetworkId;
use proto::kaswallet_proto::{FeePolicy, fee_policy};

#[derive(Parser, Debug, Clone)]
#[command(name = "kaswallet-batch-send")]
#[command(
    about = "Send KAS to many recipients through a kaspad node in one connected session (no kaswallet-daemon required)",
    long_about = None
)]
pub struct Args {
    #[arg(long, help = "Use the test network")]
    pub testnet: bool,

    #[arg(long, default_value = "10", help = "Testnet network suffix number")]
    pub testnet_suffix: u32,

    #[arg(long, help = "Use the development test network")]
    pub devnet: bool,

    #[arg(long, help = "Use the simulation test network")]
    pub simnet: bool,

    // TODO: Remove when wallet is more stable
    #[arg(long = "enable-mainnet-pre-launch", hide = true)]
    pub enable_mainnet_pre_launch: bool,

    #[arg(long = "keys", short = 'k', help = "Path to keys file")]
    pub keys_file_path: Option<String>,

    #[arg(
        long = "server",
        short = 's',
        help = "Kaspad gRPC endpoint (defaults to localhost with the network's default port)"
    )]
    pub server: Option<String>,

    /// Recipient as <address>:<amount-KAS> (e.g. kaspatest:qq...:1.5); repeatable
    #[arg(
        long = "output",
        short = 'o',
        required_unless_present_any = ["outputs_file", "show_address", "show_balance"]
    )]
    pub outputs: Vec<String>,

    /// Path to a JSON file with recipients as a map of "address":
    /// "amount-KAS" STRING entries, e.g. {"kaspatest:qq...": "1.5"}.
    /// Amounts must be strings (JSON numbers are floats and cannot
    /// represent all KAS values exactly). Mutually exclusive with -o.
    #[arg(
        long = "outputs-file",
        short = 'F',
        conflicts_with_all = ["outputs", "show_address", "show_balance"]
    )]
    pub outputs_file: Option<String>,

    /// Print the wallet's receive address (external index 0) and exit —
    /// offline: derived from the keys file's public key, no node or
    /// password needed. Combinable with --show-balance.
    #[arg(long = "show-address", conflicts_with = "outputs")]
    pub show_address: bool,

    /// Print the wallet's spendable and total balance and exit — no
    /// password needed, but requires a reachable node (--server).
    /// Combinable with --show-address.
    #[arg(long = "show-balance", conflicts_with = "outputs")]
    pub show_balance: bool,

    /// Maximum fee rate in Sompi/gram
    #[arg(long = "fee-rate-max", conflicts_with_all = ["exact_fee_rate", "max_fee"])]
    pub max_fee_rate: Option<f64>,

    /// Exact fee rate in Sompi/gram
    #[arg(long = "fee-rate-exact", conflicts_with_all = ["max_fee_rate", "max_fee"])]
    pub exact_fee_rate: Option<f64>,

    /// Maximum fee in Sompi
    #[arg(long = "fee-max", conflicts_with_all = ["max_fee_rate", "exact_fee_rate"])]
    pub max_fee: Option<u64>,

    /// Wallet password. Precedence: this flag, then the KASWALLET_PASSWORD
    /// environment variable, then an interactive prompt.
    #[arg(
        short = 'p',
        long = "password",
        env = "KASWALLET_PASSWORD",
        hide_env_values = true
    )]
    pub password: Option<String>,

    /// Use an existing change address instead of generating a new one
    #[arg(short = 'u', long = "use-existing-change-address")]
    pub use_existing_change_address: bool,

    /// Cap on recipient outputs packed into a single transaction. When more
    /// recipients are requested than fit (KIP-9 storage mass), the send is
    /// split into a chain of transactions submitted in one connected
    /// session (each spends the previous transaction's change). The tool
    /// auto-shrinks below this cap as the mass limit requires.
    #[arg(long = "max-outputs-per-tx", default_value = "100", value_parser = clap::value_parser!(u32).range(1..))]
    pub max_outputs_per_tx: u32,

    /// Build and log the full plan without signing or submitting
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Proceed even when the kaspad node reports it is not synced (e.g. isolated simnet nodes)
    #[arg(long = "allow-unsynced-node")]
    pub allow_unsynced_node: bool,

    /// Debug-level logs
    #[arg(short = 'v', long = "verbose", conflicts_with = "quiet")]
    pub verbose: bool,

    /// Quiet logs: warnings and errors only — the stdout summary is
    /// unaffected. Made for scripted loops over many invocations.
    #[arg(short = 'q', long = "quiet", conflicts_with = "verbose")]
    pub quiet: bool,
}

impl Args {
    pub fn network_id(&self) -> NetworkId {
        common::args::parse_network_type(
            self.testnet,
            self.devnet,
            self.simnet,
            self.testnet_suffix,
            self.enable_mainnet_pre_launch,
        )
    }

    pub fn fee_policy(&self) -> Option<FeePolicy> {
        if let Some(rate) = self.exact_fee_rate {
            Some(FeePolicy {
                fee_policy: Some(fee_policy::FeePolicy::ExactFeeRate(rate)),
            })
        } else if let Some(rate) = self.max_fee_rate {
            Some(FeePolicy {
                fee_policy: Some(fee_policy::FeePolicy::MaxFeeRate(rate)),
            })
        } else {
            self.max_fee.map(|fee| FeePolicy {
                fee_policy: Some(fee_policy::FeePolicy::MaxFee(fee)),
            })
        }
    }
}

// Test/embedding construction convenience (clap fills every field when
// parsing; integration tests build Args directly).
impl Default for Args {
    fn default() -> Self {
        Self {
            testnet: false,
            testnet_suffix: 10,
            devnet: false,
            simnet: false,
            enable_mainnet_pre_launch: false,
            keys_file_path: None,
            server: None,
            outputs: vec![],
            outputs_file: None,
            show_address: false,
            show_balance: false,
            max_fee_rate: None,
            exact_fee_rate: None,
            max_fee: None,
            password: None,
            use_existing_change_address: false,
            max_outputs_per_tx: 100,
            dry_run: false,
            allow_unsynced_node: false,
            verbose: false,
            quiet: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_required() {
        assert!(Args::try_parse_from(["kaswallet-batch-send", "--simnet"]).is_err());
    }

    #[test]
    fn outputs_repeat_and_network_resolves() {
        let args = Args::try_parse_from([
            "kaswallet-batch-send",
            "--simnet",
            "-o",
            "a:1",
            "--output",
            "b:2",
        ])
        .unwrap();
        assert_eq!(args.outputs, vec!["a:1".to_string(), "b:2".to_string()]);
        assert_eq!(args.network_id().to_string(), "simnet");
    }

    #[test]
    fn show_flags_need_no_outputs_and_conflict_with_them() {
        let args =
            Args::try_parse_from(["kaswallet-batch-send", "--simnet", "--show-address"]).unwrap();
        assert!(args.show_address);
        assert!(args.outputs.is_empty());

        let args =
            Args::try_parse_from(["kaswallet-batch-send", "--simnet", "--show-balance"]).unwrap();
        assert!(args.show_balance);

        let args = Args::try_parse_from([
            "kaswallet-batch-send",
            "--simnet",
            "--show-address",
            "--show-balance",
        ])
        .unwrap();
        assert!(args.show_address && args.show_balance);

        for flag in ["--show-address", "--show-balance"] {
            assert!(
                Args::try_parse_from(["kaswallet-batch-send", "--simnet", flag, "-o", "a:1",])
                    .is_err()
            );
        }
    }

    #[test]
    fn outputs_file_satisfies_requirement_and_conflicts() {
        let args = Args::try_parse_from([
            "kaswallet-batch-send",
            "--simnet",
            "-F",
            "/tmp/payouts.json",
        ])
        .unwrap();
        assert_eq!(args.outputs_file.as_deref(), Some("/tmp/payouts.json"));
        assert!(args.outputs.is_empty());

        for conflicting in [
            vec!["-o", "a:1"],
            vec!["--show-address"],
            vec!["--show-balance"],
        ] {
            let mut argv = vec!["kaswallet-batch-send", "--simnet", "-F", "p.json"];
            argv.extend(conflicting);
            assert!(Args::try_parse_from(argv).is_err());
        }
    }

    #[test]
    fn max_outputs_per_tx_defaults_and_rejects_zero() {
        let args = Args::try_parse_from(["kaswallet-batch-send", "--simnet", "-o", "a:1"]).unwrap();
        assert_eq!(args.max_outputs_per_tx, 100);

        let args = Args::try_parse_from([
            "kaswallet-batch-send",
            "--simnet",
            "-o",
            "a:1",
            "--max-outputs-per-tx",
            "1",
        ])
        .unwrap();
        assert_eq!(args.max_outputs_per_tx, 1);

        assert!(
            Args::try_parse_from([
                "kaswallet-batch-send",
                "--simnet",
                "-o",
                "a:1",
                "--max-outputs-per-tx",
                "0",
            ])
            .is_err()
        );
    }

    #[test]
    fn quiet_and_verbose_conflict() {
        assert!(
            Args::try_parse_from(["kaswallet-batch-send", "--simnet", "-o", "a:1", "-q", "-v"])
                .is_err()
        );
        let args =
            Args::try_parse_from(["kaswallet-batch-send", "--simnet", "-o", "a:1", "-q"]).unwrap();
        assert!(args.quiet && !args.verbose);
    }

    #[test]
    fn fee_flags_conflict() {
        assert!(
            Args::try_parse_from([
                "kaswallet-batch-send",
                "--simnet",
                "-o",
                "a:1",
                "--fee-rate-exact",
                "2.0",
                "--fee-max",
                "1000",
            ])
            .is_err()
        );
    }

    #[test]
    fn fee_policy_maps_flags() {
        let exact = Args {
            exact_fee_rate: Some(2.5),
            ..Args::default()
        };
        assert!(matches!(
            exact.fee_policy().unwrap().fee_policy,
            Some(fee_policy::FeePolicy::ExactFeeRate(r)) if r == 2.5
        ));

        let max_rate = Args {
            max_fee_rate: Some(3.0),
            ..Args::default()
        };
        assert!(matches!(
            max_rate.fee_policy().unwrap().fee_policy,
            Some(fee_policy::FeePolicy::MaxFeeRate(r)) if r == 3.0
        ));

        let max_fee = Args {
            max_fee: Some(1_000),
            ..Args::default()
        };
        assert!(matches!(
            max_fee.fee_policy().unwrap().fee_policy,
            Some(fee_policy::FeePolicy::MaxFee(1_000))
        ));

        assert!(Args::default().fee_policy().is_none());
    }
}
