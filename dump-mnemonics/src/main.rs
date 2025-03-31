use clap::Parser;
use kaspa_bip32::Prefix;
use common::args::calculate_path;
use common::keys::Keys;

mod args;

fn main() {
    let args = args::Args::parse();
    let network_id = args.network_id();
    let keys_file_path = calculate_path(args.keys_file.clone(), network_id, "keys.json");
    let extended_keys_prefix = Prefix::from(network_id);
    let keys = Keys::load(&keys_file_path, extended_keys_prefix).expect("Failed to load keys");

    println!("Please enter password:");
    let password = rpassword::read_password().unwrap();

    let mnemonics = keys.decrypt_mnemonics(&password);
    if let Err(e) = mnemonics{
        println!("Failed to decrypt mnemonics: {}", e);
        return
    }
    let mnemonics = mnemonics.unwrap();

    println!("Decrypted mnemonics:");

    for mnemonic in mnemonics {
        println!("{:#?}", mnemonic.phrase_string());
    }
}
