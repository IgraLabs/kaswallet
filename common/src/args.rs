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

pub fn calculate_path(
    args_file_path: &Option<String>,
    network_id: &NetworkId,
    default_filename: &str,
) -> String {
    let path = if let Some(path) = args_file_path {
        return path.to_string();
    } else if cfg!(target_os = "windows") {
        format!(
            "%USERPROFILE%\\AppData\\Local\\Kaswallet\\{}\\{}",
            network_id, default_filename
        )
    } else {
        format!("~/.kaswallet/{}/{}", network_id, default_filename)
    };

    expand_path(&path)
}

fn expand_path(path: &str) -> String {
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
