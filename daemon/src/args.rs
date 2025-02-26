use clap::Parser;
use kaspa_consensus_core::network::NetworkId;

#[derive(Parser, Debug)]
#[command(name = "kaswallet-create")]
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
    #[arg(long, short = 'k', default_value = core::args::default_keys_path(), help="Path to keys file")]
    keys_file: String,

    #[arg(long, short = 's', help = "Kaspa node RPC server to connect to")]
    server: Option<String>,

    #[arg(
        long,
        short = 'l',
        default_value = "127.0.0.1:8082",
        help = "Address to listen on"
    )]
    listen: String,
}

impl Args {
    pub fn network(&self) -> NetworkId {
        core::args::parse_network_type(self.testnet, self.devnet, self.simnet, self.testnet_suffix)
    }
}
