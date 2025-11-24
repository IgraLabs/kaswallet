use kaswallet_test_helpers::mnemonics::create_known_test_mnemonic;
use kaswallet_test_helpers::start_daemon::{start_kaspad, start_wallet_daemon};
use proto::kaswallet_proto::wallet_client::WalletClient;
use proto::kaswallet_proto::GetAddressesRequest;
use rstest::rstest;
use tonic::Request;

#[rstest]
#[tokio::test]
pub async fn test_p2pk() {
    let mnemnonic = create_known_test_mnemonic();

    let (_keys, keys_file_path) =
        kaswallet_test_helpers::create::create_keys_file(mnemnonic).unwrap();
    let (mut kaspad_daemon, kaspad_client) = start_kaspad().await;
    let (wallet_daemon, listen) = start_wallet_daemon(kaspad_client, keys_file_path).await;

    let mut wallet_client = WalletClient::connect(listen).await.unwrap();
    wallet_client
        .get_addresses(Request::new(GetAddressesRequest {}))
        .await;

    kaspad_daemon.shutdown();
}
