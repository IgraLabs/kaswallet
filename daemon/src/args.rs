use clap::{Parser, ValueEnum};
use kaspa_consensus_core::network::NetworkId;
use log::LevelFilter;

#[derive(Parser, Debug)]
#[command(name = "kaswallet-daemon")]
pub struct Args {
    #[arg(long, help = "Use the test network")]
    testnet: bool,

    #[arg(long, default_value = "10", help = "Testnet network suffix number")]
    testnet_suffix: u32,

    #[arg(long, help = "Use the development test network")]
    devnet: bool,

    #[arg(long, help = "Use the simulation test network")]
    simnet: bool,

    /// Path to keys.json (default: ~/.kwallet/keys.json)
    #[arg(long, short = 'k', default_value = core::args::default_keys_path(), help="Path to keys file"
    )]
    pub keys_file: String,

    #[arg(long, default_value = core::args::default_logs_path(), help="Path to logs directory")]
    pub logs_path: String,

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

impl Into<LevelFilter> for LogsLevel {
    fn into(self) -> LevelFilter {
        match self {
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
    pub fn network(&self) -> NetworkId {
        core::args::parse_network_type(self.testnet, self.devnet, self.simnet, self.testnet_suffix)
    }
}
