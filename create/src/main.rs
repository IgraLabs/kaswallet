use clap::Parser;
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

fn main() {
    let args = args::Args::parse();
    let is_multisig = args.num_public_keys > 1;

    let mut mnemonics = vec![];
    for i in 0..args.num_private_keys {
        let mnemonic: Mnemonic;
        let mnemonic = if args.import {
            mnemonic = prompt_for_mnemonic();
        } else {
            mnemonic = Mnemonic::random(WordCount::Words24, Language::English).unwrap();
            println!("Mnemonic #{}:\n{}\n\n", i + 1, mnemonic.phrase());
        };
        mnemonics.push(mnemonic);
    }

    println!("{:?}", args);
}
