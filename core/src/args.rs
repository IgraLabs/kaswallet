use kaspa_consensus_core::network::{NetworkId, NetworkType};

pub fn parse_network_type(
    testnet: bool,
    devnet: bool,
    simnet: bool,
    testnet_suffix: u32,
) -> NetworkId {
    match (testnet, devnet, simnet) {
        (false, false, false) => NetworkId::new(NetworkType::Mainnet),
        (true, false, false) => NetworkId::with_suffix(NetworkType::Testnet, testnet_suffix),
        (false, true, false) => NetworkId::new(NetworkType::Devnet),
        (false, false, true) => NetworkId::new(NetworkType::Simnet),
        _ => panic!("only a single net should be activated"),
    }
}

pub fn default_keys_path() -> &'static str {
    if cfg!(target_os = "windows") {
        "%USERPROFILE%\\AppData\\Local\\Kaspawallet\\key.json"
    } else {
        "~/.kwallet/keys.json"
    }
}
