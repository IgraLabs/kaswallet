use crate::args::Args;
use clap::Parser;
use constant_time_eq::constant_time_eq;
use kaspa_bip32::mnemonic::Mnemonic;
use kaspa_bip32::{Language, WordCount};
use std::io;

mod args;

pub fn prompt_for_mnemonic() -> Mnemonic {
    loop {
        println!("Please enter mnemonic (24 space separated words):");
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let input = input.trim(); // trim trailing chars that read_line adds.

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

fn get_mnemonics(args: &Args) -> Vec<Mnemonic> {
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

    let mnemonics = get_mnemonics(&args);
    let password = prompt_for_password();

    for (i, mnemonic) in mnemonics.iter().enumerate() {
        println!("Mnemonic #{}: {}", i + 1, mnemonic.phrase());
    }
    println!("Password: {}", password);
}
