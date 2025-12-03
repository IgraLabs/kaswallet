use clap::Parser;
use kaspa_consensus_core::network::NetworkId;

#[derive(Parser, Debug)]
#[command(name = "kaswallet-create")]
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

    /// Import from mnemonic rather than create new
    #[arg(
        long,
        short = 'i',
        help = "Import private keys from mnemonic rather than generating new ones"
    )]
    pub import: bool,

    #[arg(long, default_value_t = 1, help = "Minimum number of signatures")]
    pub min_signatures: u16,

    #[arg(long, default_value_t = 1, help = "Number of private keys")]
    pub num_private_keys: u16,

    #[arg(long, default_value_t = 1, help = "Number of public keys")]
    pub num_public_keys: u16,
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

impl Default for Args {
    fn default() -> Self {
        Self {
            testnet: false,
            testnet_suffix: 10,
            devnet: false,
            simnet: false,
            enable_mainnet_pre_launch: false,
            keys_file_path: None,
            import: false,
            min_signatures: 1,
            num_private_keys: 1,
            num_public_keys: 1,
        }
    }
}
