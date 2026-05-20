use clap::{Parser, ValueEnum};
use common::args::parse_network_type;
use common::error_location::ErrorLocation;
use common::errors::{UserInputError, WalletError, WalletResult};
use kaspa_consensus_core::network::NetworkId;
use kaspa_consensus_core::subnets::{SUBNETWORK_ID_NATIVE, SubnetworkId};
use std::str::FromStr;
use tracing_subscriber::filter::LevelFilter as TracingLevelFilter;

pub fn parse_subnetwork_id(arg: Option<&str>) -> WalletResult<SubnetworkId> {
    match arg {
        Some(s) => SubnetworkId::from_str(s).map_err(|e| {
            WalletError::from(UserInputError::InvalidHex {
                reason: format!("--subnetwork-id: {e}"),
                location: ErrorLocation::capture(),
            })
        }),
        None => Ok(SUBNETWORK_ID_NATIVE),
    }
}

#[derive(Parser, Debug, Clone)]
#[command(name = "kaswallet-daemon")]
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

    #[arg(long, help = "Path to logs directory")]
    pub logs_path: Option<String>,

    #[arg(long, short = 'v', default_value = "info", help = "Log level")]
    pub logs_level: LogsLevel,

    #[arg(long, short = 's', help = "Kaspa node RPC server to connect to")]
    pub server: Option<String>,

    #[arg(
        long,
        short = 'l',
        default_value = "127.0.0.1:8082",
        help = "Address to listen on"
    )]
    pub listen: String,

    #[arg(
        long,
        help = "Custom subnetwork ID as 40 hex chars (omit for native subnetwork)"
    )]
    pub subnetwork_id: Option<String>,

    #[arg(long, help = "Enable tokio console")]
    #[cfg(debug_assertions)]
    pub enable_tokio_console: bool,

    #[arg(
        long,
        default_value = "10000",
        help = "Sync interval in milliseconds",
        hide = true
    )]
    pub sync_interval_millis: u64,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            testnet: false,
            testnet_suffix: 10,
            devnet: false,
            simnet: false,
            enable_mainnet_pre_launch: false,
            keys_file_path: None,
            logs_path: None,
            logs_level: Default::default(),
            server: None,
            listen: "".to_string(),
            subnetwork_id: None,
            #[cfg(debug_assertions)]
            enable_tokio_console: false,
            sync_interval_millis: 10,
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Default)]
pub enum LogsLevel {
    Off,
    Trace,
    #[default]
    Debug,
    Info,
    Warn,
    Error,
}

impl From<&LogsLevel> for TracingLevelFilter {
    fn from(value: &LogsLevel) -> TracingLevelFilter {
        match value {
            LogsLevel::Off => TracingLevelFilter::OFF,
            LogsLevel::Trace => TracingLevelFilter::TRACE,
            LogsLevel::Debug => TracingLevelFilter::DEBUG,
            LogsLevel::Info => TracingLevelFilter::INFO,
            LogsLevel::Warn => TracingLevelFilter::WARN,
            LogsLevel::Error => TracingLevelFilter::ERROR,
        }
    }
}

impl Args {
    pub fn network_id(&self) -> NetworkId {
        parse_network_type(
            self.testnet,
            self.devnet,
            self.simnet,
            self.testnet_suffix,
            self.enable_mainnet_pre_launch,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_subnetwork_id_flag_is_parsed() {
        let args = Args::try_parse_from([
            "kaswallet-daemon",
            "--subnetwork-id",
            "97b1000000000000000000000000000000000000",
        ])
        .expect("clap should parse a valid subnetwork-id flag");
        assert_eq!(
            args.subnetwork_id.as_deref(),
            Some("97b1000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn args_subnetwork_id_default_is_none() {
        let args =
            Args::try_parse_from(["kaswallet-daemon"]).expect("clap should parse with no flags");
        assert!(args.subnetwork_id.is_none(), "default must be None");
    }

    #[test]
    fn parse_subnetwork_id_returns_native_when_unset() {
        let parsed = parse_subnetwork_id(None).expect("None should resolve to NATIVE");
        assert_eq!(parsed, SUBNETWORK_ID_NATIVE);
    }

    #[test]
    fn parse_subnetwork_id_parses_valid_hex() {
        let parsed = parse_subnetwork_id(Some("97b1000000000000000000000000000000000000"))
            .expect("valid 40-char hex should parse");
        let bytes: &[u8] = parsed.as_ref();
        assert_eq!(bytes[0], 0x97);
        assert_eq!(bytes[1], 0xb1);
        assert!(bytes[2..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parse_subnetwork_id_rejects_short_hex() {
        let err = parse_subnetwork_id(Some("97b10000"))
            .expect_err("anything shorter than 40 chars must be rejected");
        assert!(
            matches!(
                err,
                WalletError::UserInput(UserInputError::InvalidHex { .. })
            ),
            "expected InvalidHex, got: {err}"
        );
    }

    #[test]
    fn parse_subnetwork_id_rejects_non_hex() {
        let err = parse_subnetwork_id(Some("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"))
            .expect_err("non-hex must be rejected");
        assert!(
            matches!(
                err,
                WalletError::UserInput(UserInputError::InvalidHex { .. })
            ),
            "expected InvalidHex, got: {err}"
        );
    }
}
