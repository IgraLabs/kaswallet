use clap::{Parser, ValueEnum};
use common::args::parse_network_type;
use kaspa_consensus_core::network::NetworkId;
use log::LevelFilter;

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

impl From<LogsLevel> for LevelFilter {
    fn from(value: LogsLevel) -> LevelFilter {
        match value {
            LogsLevel::Off => LevelFilter::Off,
            LogsLevel::Trace => LevelFilter::Trace,
            LogsLevel::Debug => LevelFilter::Debug,
            LogsLevel::Info => LevelFilter::Info,
            LogsLevel::Warn => LevelFilter::Warn,
            LogsLevel::Error => LevelFilter::Error,
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
