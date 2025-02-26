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

    /// Import from mnemonic rather than create new
    #[arg(
        long,
        short = 'i',
        help = "Import private keys from mnemonic rather than generating new ones"
    )]
    import: bool,

    #[arg(long, default_value_t = 1, help = "Minimum number of signatures")]
    min_signatures: u16,

    #[arg(long, default_value_t = 1, help = "Number of private keys")]
    num_private_keys: u16,

    #[arg(long, default_value_t = 1, help = "Number of public keys")]
    num_public_keys: u16,
}

impl Args {
    pub fn network(&self) -> NetworkId {
        core::args::parse_network_type(self.testnet, self.devnet, self.simnet, self.testnet_suffix)
    }
}
