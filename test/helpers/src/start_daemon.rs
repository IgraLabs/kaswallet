use kaspa_consensus_core::config::params::SIMNET_PARAMS;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_grpc_client::GrpcClient;
use kaspa_testing_integration::common::daemon::Daemon as KaspadDaemon;
use kaspa_utils::fd_budget;
use kaspad_lib::args::Args as KaspadArgs;
use kaswallet_daemon::Daemon;
use kaswallet_daemon::args::{Args, parse_subnetwork_id_arg};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn p2pk_test() {}

fn pick_unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
    // Dropping listener here frees the port (eventually)
}

async fn start_wallet_daemon_inner(
    kaspad_client: Arc<GrpcClient>,
    keys_file_path: String,
    subnetwork_id: Option<SubnetworkId>,
) -> (Daemon, String) {
    let port = pick_unused_port();
    let listen = format!("127.0.0.1:{}", port);
    let args = Arc::new(Args {
        keys_file_path: Some(keys_file_path),
        simnet: true,
        listen: listen.clone(),
        sync_interval_millis: 500,
        subnetwork_id,
        ..Default::default()
    });
    let mut params = SIMNET_PARAMS.clone();
    // Upstream collapsed `prior_coinbase_maturity` and `crescendo.coinbase_maturity`
    // into a single `coinbase_maturity` on the Toccata branch.
    params.coinbase_maturity = 0;

    let daemon = Daemon::new(args);
    daemon
        .start_with_kaspad_client_and_consensus_params(kaspad_client, params)
        .await
        .expect("failed to start wallet daemon");

    (daemon, listen)
}

pub async fn start_wallet_daemon(
    kaspad_client: Arc<GrpcClient>,
    keys_file_path: String,
) -> (Daemon, String) {
    start_wallet_daemon_inner(kaspad_client, keys_file_path, None).await
}

pub async fn start_wallet_daemon_with_subnetwork_id(
    kaspad_client: Arc<GrpcClient>,
    keys_file_path: String,
    subnetwork_namespace_hex: &str,
) -> (Daemon, String) {
    let subnetwork_id = parse_subnetwork_id_arg(subnetwork_namespace_hex)
        .expect("test must pass a well-formed 4-byte lane namespace");
    start_wallet_daemon_inner(kaspad_client, keys_file_path, Some(subnetwork_id)).await
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
