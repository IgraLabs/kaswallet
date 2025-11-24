use common::errors::WalletResult;
use common::keys::Keys;
use kaspa_bip32::Mnemonic;
use kaswallet_create::args::Args;
use kaswallet_create::generate_keys_file::generate_keys_file;
use std::sync::Arc;
use tempfile::NamedTempFile;

pub fn create_keys_file(mnemnonic: Mnemonic) -> WalletResult<(Keys, String)> {
    let keys_file_path = NamedTempFile::with_suffix(".json")
        .unwrap()
        .path()
        .to_string_lossy()
        .to_string();
    let create_args = Arc::new(Args {
        simnet: true,
        keys_file_path: Some(keys_file_path.clone()),
        ..Default::default()
    });
    let keys_file = generate_keys_file(
        create_args,
        keys_file_path.clone(),
        Arc::new(vec![mnemnonic.clone()]),
        "".to_string(),
        vec![],
    )?;

    Ok((keys_file, keys_file_path))
}
