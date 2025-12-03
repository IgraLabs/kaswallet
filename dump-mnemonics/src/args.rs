use clap::Parser;
use kaspa_consensus_core::network::NetworkId;

#[derive(Parser, Debug)]
#[command(name = "kaswallet-dump-mnemonics")]
pub struct Args {
    #[arg(long, help = "Use the test network")]
    testnet: bool,

    #[arg(long, default_value = "10", help = "Testnet network suffix number")]
    testnet_suffix: u32,

    #[arg(long, help = "Use the development test network")]
    devnet: bool,

    #[arg(long, help = "Use the simulation test network")]
    simnet: bool,

    // TODO: Remove when wallet is more stable
    #[arg(long = "enable-mainnet-pre-launch", hide = true)]
    pub enable_mainnet_pre_launch: bool,

    #[arg(long = "keys", short = 'k', help = "Path to keys file")]
    pub keys_file_path: Option<String>,
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
}
