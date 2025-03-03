use kaspa_consensus_core::network::{NetworkId, NetworkType};
use regex::Regex;
use std::env;

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
        "%USERPROFILE%\\AppData\\Local\\Kaswallet\\key.json"
    } else {
        "~/.kaswallet/keys.json"
    }
}

pub fn default_logs_path() -> &'static str {
    if cfg!(target_os = "windows") {
        "%USERPROFILE%\\AppData\\Local\\Kaswallet\\logs"
    } else {
        "~/.kaswallet/logs"
    }
}

pub fn expand_path(path: String) -> String {
    if cfg!(target_os = "windows") {
        let re = Regex::new(r"%([^%]+)%").unwrap();
        re.replace_all(&path, |caps: &regex::Captures| {
            env::var(&caps[1]).unwrap_or_else(|_| caps[0].to_string())
        })
        .to_string()
    } else {
        shellexpand::tilde(&path).to_string()
    }
}
