use kaspa_bip32::Language;
use kaspa_bip32::Mnemonic;

// Helper: Known valid 24-word mnemonic with duplicate word ("letter")
pub fn create_known_test_mnemonic() -> Mnemonic {
    let phrase = "decade minimum language dutch option narrow negative weird ball garbage purity guide weapon juice melt trash theory memory warrior rural okay flavor erosion senior";
    Mnemonic::new(phrase, Language::English).unwrap()
}
