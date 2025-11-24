use daemon::args::Args;
use daemon::Daemon;
use kaspa_grpc_client::GrpcClient;
use kaspa_testing_integration::common::daemon;
use kaspa_testing_integration::common::daemon::Daemon as KaspadDaemon;
use kaspa_utils::fd_budget;
use kaspad_lib::args::Args as KaspadArgs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::NamedTempFile;

#[tokio::test]
async fn p2pk_test() {
    let (mut daemon, kaspad_client) = start_kaspad().await;


    daemon.shutdown();
}

async fn start_wallet_daemon(kaspad_client: GrpcClient) -> Daemon {
    let args = Args {
        simnet: true,
        keys_file_path: NamedTempFile::with_suffix(".json").unwrap(),
    };

    let daemon = Daemon::new(args);
    daemon.start_with_client(Arc::new(kaspad_client)).await.unwrap()
}

async fn start_kaspad() -> (KaspadDaemon, GrpcClient) {
    let override_params_file = Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("override_params.json")
            .to_string_lossy()
            .to_string(),
    );
    let args = KaspadArgs {
        simnet: true,
        disable_upnp: true,
        enable_unsynced_mining: true,
        utxoindex: true,
        override_params_file,
        unsafe_rpc: true,
        ..Default::default()
    };

    let fd_total_budget = fd_budget::limit();
    let mut daemon = KaspadDaemon::new_random_with_args(args, fd_total_budget);
    let kaspad_client = daemon.start().await;

    (daemon, kaspad_client)
}
