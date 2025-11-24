use crate::args::Args;
use clap::Parser;
use common::args::calculate_path;
use constant_time_eq::constant_time_eq;
use kaspa_bip32::mnemonic::Mnemonic;
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{ExtendedPublicKey, Language, WordCount};
use kaswallet_create::args;
use kaswallet_create::generate_keys_file::generate_keys_file;
use kaswallet_create::helpers::read_line;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

fn main() {
    let args = Arc::new(args::Args::parse());
    let network_id = args.network_id();
    let keys_file_path = calculate_path(&args.keys_file_path, &network_id, "keys.json");
    if !should_continue_if_key_file_exists(&keys_file_path) {
        return;
    }

    let password = prompt_for_password();
    let mnemonics = prompt_or_generate_mnemonics(args.clone());
    let extra_public_keys = prompt_for_extra_public_keys(args.clone(), mnemonics.clone());

    let keys_file = match generate_keys_file(
        args.clone(),
        keys_file_path,
        mnemonics,
        password,
        extra_public_keys,
    ) {
        Ok(keys) => keys,
        Err(e) => {
            println!("{}", e);
            return;
        }
    };

    println!("Keys data written to {}", keys_file.file_path);
}

fn prompt_for_extra_public_keys(
    args: Arc<Args>,
    mnemonics: Arc<Vec<Mnemonic>>,
) -> Vec<ExtendedPublicKey<PublicKey>> {
    let mut extra_public_keys: Vec<ExtendedPublicKey<PublicKey>> = vec![];
    let mnemonics_count = mnemonics.len() as u16;
    for i in mnemonics_count..args.num_public_keys {
        let x_public_key = prompt_for_x_public_key(i as usize);
        extra_public_keys.push(x_public_key);
    }
    extra_public_keys
}
fn prompt_for_x_public_key(i: usize) -> ExtendedPublicKey<PublicKey> {
    println!("enter extended public key #{}:", i + 1);
    let input = read_line();
    let x_public_key = ExtendedPublicKey::from_str(&input);
    x_public_key.unwrap()
}

pub fn prompt_for_mnemonic() -> Mnemonic {
    loop {
        println!("Please enter mnemonic (24 space separated words):");
        let input = read_line();

        let list = input
            .split_whitespace()
            .map(|s| s.to_string())
            .collect::<Vec<String>>();
        if list.len() != 24 {
            println!("Mnemonic must be exactly 24 words!");
            continue;
        }

        let mnemonic = Mnemonic::new(input, Language::English);
        if mnemonic.is_err() {
            println!("Invalid mnemonic: {}", mnemonic.err().unwrap());
            continue;
        }

        return mnemonic.unwrap();
    }
}

fn prompt_for_password() -> String {
    loop {
        println!("Please enter encryption password:");
        let password = rpassword::read_password().unwrap();
        println!("Please confirm your password");
        let confirm_password = rpassword::read_password().unwrap();

        if !constant_time_eq(password.as_bytes(), confirm_password.as_bytes()) {
            println!("Passwords do not match!");
            continue;
        }

        return password;
    }
}

fn prompt_or_generate_mnemonics(args: Arc<Args>) -> Arc<Vec<Mnemonic>> {
    let mut mnemonics: Vec<Mnemonic> = vec![];
    for i in 0..args.num_private_keys {
        let mnemonic: Mnemonic = if args.import {
            prompt_for_mnemonic()
        } else {
            let random_mnemonic = Mnemonic::random(WordCount::Words24, Language::English).unwrap();
            println!("Mnemonic #{}:\n{}\n\n", i + 1, random_mnemonic.phrase());
            random_mnemonic
        };
        mnemonics.push(mnemonic);
    }
    Arc::new(mnemonics)
}

fn should_continue_if_key_file_exists(keys_file_path: &str) -> bool {
    if Path::new(keys_file_path).exists() {
        println!(
            "Keys file already exists at {}. Do you wish to overwrite it? (type 'yes' if you do)",
            keys_file_path
        );
        let input = read_line();
        return input == "yes";
    }
    true
}
