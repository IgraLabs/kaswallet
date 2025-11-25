use kaspa_consensus_core::config::params::SIMNET_PARAMS;
use kaspa_grpc_client::GrpcClient;
use kaspa_testing_integration::common::daemon::Daemon as KaspadDaemon;
use kaspa_utils::fd_budget;
use kaspad_lib::args::Args as KaspadArgs;
use kaswallet_daemon::args::Args;
use kaswallet_daemon::Daemon;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn p2pk_test() {}

fn pick_unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Dropping listener here frees the port (eventually)
    port
}

pub async fn start_wallet_daemon(
    kaspad_client: Arc<GrpcClient>,
    keys_file_path: String,
) -> (Daemon, String) {
    let port = pick_unused_port();
    let listen = format!("127.0.0.1:{}", port);
    let args = Arc::new(Args {
        keys_file_path: Some(keys_file_path),
        simnet: true,
        listen: listen.clone(),
        sync_interval_millis: 500,
        ..Default::default()
    });
    let mut params = SIMNET_PARAMS.clone();
    params.prior_coinbase_maturity = 0;
    params.crescendo.coinbase_maturity = 0;

    let daemon = Daemon::new(args);
    daemon
        .start_with_kaspad_client_and_consensus_params(kaspad_client, params)
        .await
        .unwrap();

    (daemon, listen)
}

pub async fn start_kaspad() -> (KaspadDaemon, Arc<GrpcClient>) {
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
        appdir: tempfile::tempdir()
            .unwrap()
            .path()
            .to_str()
            .map(|s| s.to_string()),
        ..Default::default()
    };

    let fd_total_budget = fd_budget::limit();
    let mut daemon = KaspadDaemon::new_random_with_args(args, fd_total_budget);
    let kaspad_client = daemon.start().await;

    (daemon, Arc::new(kaspad_client))
}
