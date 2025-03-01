use crate::args::Args;
use clap::Parser;
use constant_time_eq::constant_time_eq;
use core::encrypted_mnemonic::EncryptedMnemonic;
use kaspa_bip32::mnemonic::Mnemonic;
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{
    secp256k1, ExtendedPrivateKey, ExtendedPublicKey, Language, Prefix, PrivateKey, SecretKey,
    SecretKeyExt, WordCount,
};
use kaspa_wallet_keys::derivation::gen1::WalletDerivationManager;
use std::io;
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

fn prompt_for_x_public_key(i: usize, args: &Args) -> ExtendedPublicKey<PublicKey> {
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

fn main() {
    let args = args::Args::parse();
    let is_multisig = args.num_public_keys > 1;

    let password = prompt_for_password();
    let mnemonics = prompt_or_generate_mnemonics(&args);

    let encrypted_mnemonics: Vec<EncryptedMnemonic> = mnemonics
        .iter()
        .map(|mnemonic| EncryptedMnemonic::new(mnemonic, &password))
        .collect();
    let x_private_keys: Vec<ExtendedPrivateKey<SecretKey>> = mnemonics
        .iter()
        .map(|mnemonic: &Mnemonic| {
            let seed = mnemonic.to_seed("");
            ExtendedPrivateKey::new(seed).unwrap()
        })
        .collect();
    let mut x_public_keys: Vec<ExtendedPublicKey<secp256k1::PublicKey>> = x_private_keys
        .iter()
        .map(|x_private_key| {
            let (key, attributes) = WalletDerivationManager::derive_extended_key_from_master_key(
                x_private_key.clone(),
                is_multisig,
                0,
            )
            .unwrap();
            ExtendedPublicKey {
                public_key: key.get_public_key(),
                attrs: attributes,
            }
        })
        .collect();

    for (i, x_public_key) in x_public_keys.iter().enumerate() {
        println!(
            "Extended public key of mnemonic#{}: {}",
            i + 1,
            x_public_key.to_string(Some(Prefix::from(args.network())))
        );
    }

    while x_public_keys.len() < args.num_public_keys as usize {
        let x_public_key = prompt_for_x_public_key(x_public_keys.len(), &args);
        x_public_keys.push(x_public_key);
    }
}
