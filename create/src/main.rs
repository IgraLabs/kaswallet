use crate::args::Args;
use clap::Parser;
use common::args::calculate_path;
use common::encrypted_mnemonic::EncryptedMnemonic;
use common::keys::{Keys, KEY_FILE_VERSION};
use constant_time_eq::constant_time_eq;
use kaspa_bip32::mnemonic::Mnemonic;
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{ExtendedPrivateKey, ExtendedPublicKey, Language, Prefix, SecretKey, WordCount};
use std::io;
use std::path::Path;
use std::str::FromStr;

mod args;

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

fn read_line() -> String {
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let input = input.trim().to_string(); // trim trailing chars that read_line adds.
    input
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

fn prompt_for_x_public_key(i: usize) -> ExtendedPublicKey<PublicKey> {
    println!("enter extended public key #{}:", i + 1);
    let input = read_line();
    let x_public_key = ExtendedPublicKey::from_str(&input);
    x_public_key.unwrap()
}

fn prompt_or_generate_mnemonics(args: &Args) -> Vec<Mnemonic> {
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
    mnemonics
}

fn minimum_cosigner_index(
    all_public_keys: Vec<ExtendedPublicKey<PublicKey>>,
    signer_public_keys: Vec<ExtendedPublicKey<PublicKey>>,
    prefix: Prefix,
) -> u16 {
    let mut sorted_public_keys = all_public_keys.clone();
    sorted_public_keys.sort_by(|a, b| a.to_string(Some(prefix)).cmp(&b.to_string(Some(prefix))));

    let mut minimum_cosigner_index = sorted_public_keys.len();
    for x_public_key in signer_public_keys {
        let current_key_cosigner_index = sorted_public_keys
            .iter()
            .position(|x| x.eq(&x_public_key))
            .unwrap_or(0);
        if current_key_cosigner_index < minimum_cosigner_index {
            minimum_cosigner_index = current_key_cosigner_index;
        }
    }

    minimum_cosigner_index as u16
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
fn main() {
    let args = args::Args::parse();
    let network_id = args.network_id();
    let keys_file_path = calculate_path(args.keys_file.clone(), network_id, "keys.json");
    if !should_continue_if_key_file_exists(&keys_file_path) {
        return;
    }

    let password = prompt_for_password();
    let mnemonics = prompt_or_generate_mnemonics(&args);

    let mut encrypted_mnemonics = vec![];
    for mnemonic in mnemonics.iter() {
        let encrypted_mnemonic = EncryptedMnemonic::new(&mnemonic, &password);
        if let Err(e) = encrypted_mnemonic {
            println!("Error encrypting mnemonic: {}", e);
            return;
        }
        encrypted_mnemonics.push(encrypted_mnemonic.unwrap());
    }
    let x_private_keys: Vec<ExtendedPrivateKey<SecretKey>> = mnemonics
        .iter()
        .map(|mnemonic: &Mnemonic| {
            let seed = mnemonic.to_seed("");
            ExtendedPrivateKey::new(seed).unwrap()
        })
        .collect();
    let x_public_keys: Vec<ExtendedPublicKey<PublicKey>> = x_private_keys
        .iter()
        .map(|x_private_key| x_private_key.public_key())
        .collect();

    let prefix = Prefix::from(args.network_id());
    for (i, x_public_key) in x_public_keys.iter().enumerate() {
        println!(
            "Extended public key of mnemonic#{}: {}",
            i + 1,
            x_public_key.to_string(Some(prefix))
        );
    }

    let mut all_public_keys = x_public_keys.clone();
    while all_public_keys.len() < args.num_public_keys as usize {
        let x_public_key = prompt_for_x_public_key(all_public_keys.len());
        all_public_keys.push(x_public_key);
    }

    let cosigner_index: u16 = if x_public_keys.len() == 0 {
        0
    } else {
        minimum_cosigner_index(all_public_keys.clone(), x_public_keys, prefix)
    };

    let keys_file = Keys::new(
        keys_file_path.clone(),
        KEY_FILE_VERSION,
        encrypted_mnemonics,
        prefix,
        all_public_keys,
        0,
        0,
        args.min_signatures,
        cosigner_index,
    );

    keys_file.save().unwrap();
    println!("Keys data written to {}", keys_file_path);
}
