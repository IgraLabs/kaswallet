use grpc_tonic::{Request as NodeRequest, Response as NodeResponse, Status as NodeStatus};
use kaspa_consensus_core::config::params::SIMNET_PARAMS;
use kaspa_grpc_client::GrpcClient;
use kaspa_grpc_core::protowire::{
    self,
    rpc_server::{Rpc, RpcServer},
};
use kaswallet_daemon::{Daemon, args::Args};
use kaswallet_test_helpers::mnemonics::create_known_test_mnemonic;
use proto::kaswallet_proto::{wallet_client::WalletClient, *};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering::SeqCst},
};
use tokio::{
    net::TcpListener,
    sync::mpsc,
    time::{Duration, sleep, timeout},
};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{Code, transport::Channel};

#[derive(Clone, Default)]
struct Node {
    // 0: healthy; 1/2/3: fail balances/mempool/UTXO sync RPC respectively.
    failed_operation: Arc<AtomicUsize>,
    failures: Arc<[AtomicUsize; 3]>,
}

impl Node {
    fn failure(&self, operation: usize) -> Option<protowire::RpcError> {
        if self.failed_operation.load(SeqCst) != operation {
            return None;
        }
        self.failures[operation - 1].fetch_add(1, SeqCst);
        Some(protowire::RpcError {
            message: if operation == 2 {
                kaspa_rpc_core::RpcError::ConsensusInTransitionalIbdState.to_string()
            } else {
                kaspa_grpc_client::error::Error::Timeout.to_string()
            },
        })
    }

    async fn wait_for_failure(&self, operation: usize, previous: usize) {
        timeout(Duration::from_secs(30), async {
            while self.failures[operation - 1].load(SeqCst) == previous {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sync did not reach the failing RPC");
    }
}

#[grpc_tonic::async_trait]
impl Rpc for Node {
    type MessageStreamStream = ReceiverStream<Result<protowire::KaspadResponse, NodeStatus>>;

    async fn message_stream(
        &self,
        request: NodeRequest<grpc_tonic::Streaming<protowire::KaspadRequest>>,
    ) -> Result<NodeResponse<Self::MessageStreamStream>, NodeStatus> {
        let mut requests = request.into_inner();
        let node = self.clone();
        let (sender, responses) = mpsc::channel(16);
        tokio::spawn(async move {
            use protowire::{kaspad_request::Payload as Q, *};
            while let Ok(Some(request)) = requests.message().await {
                let mut response: KaspadResponse = match request.payload.unwrap() {
                    Q::GetInfoRequest(_) => GetInfoResponseMessage {
                        is_synced: true,
                        is_utxo_indexed: true,
                        has_message_id: true,
                        ..Default::default()
                    }
                    .into(),
                    Q::GetBalancesByAddressesRequest(_) => GetBalancesByAddressesResponseMessage {
                        error: node.failure(1),
                        ..Default::default()
                    }
                    .into(),
                    Q::GetMempoolEntriesByAddressesRequest(_) => {
                        GetMempoolEntriesByAddressesResponseMessage {
                            error: node.failure(2),
                            ..Default::default()
                        }
                        .into()
                    }
                    Q::GetUtxosByAddressesRequest(_) => GetUtxosByAddressesResponseMessage {
                        error: node.failure(3),
                        ..Default::default()
                    }
                    .into(),
                    Q::GetBlockDagInfoRequest(_) => GetBlockDagInfoResponseMessage {
                        network_name: "kaspa-simnet".into(),
                        pruning_point_hash: "00".repeat(32),
                        sink: "00".repeat(32),
                        virtual_daa_score: 1_000_000,
                        ..Default::default()
                    }
                    .into(),
                    Q::GetFeeEstimateRequest(_) => GetFeeEstimateResponseMessage {
                        estimate: Some(RpcFeeEstimate {
                            priority_bucket: Some(RpcFeerateBucket::default()),
                            normal_buckets: vec![RpcFeerateBucket::default()],
                            ..Default::default()
                        }),
                        error: None,
                    }
                    .into(),
                    other => panic!("unexpected upstream RPC: {other:?}"),
                };
                response.id = request.id;
                if sender.send(Ok(response)).await.is_err() {
                    break;
                }
            }
        });
        Ok(NodeResponse::new(ReceiverStream::new(responses)))
    }
}

async fn assert_cache_state(wallet: &mut WalletClient<Channel>, stale: bool) {
    timeout(Duration::from_secs(30), async {
        loop {
            match wallet.get_balance(GetBalanceRequest::default()).await {
                Err(error) if stale && error.code() == Code::FailedPrecondition => break,
                Ok(_) if !stale => break,
                result => {
                    if let Err(ref error) = result {
                        assert_eq!(error.code(), Code::FailedPrecondition);
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            }
        }
    })
    .await
    .expect("wallet did not reach expected cache state");
    let utxos = wallet.get_utxos(GetUtxosRequest::default()).await;
    if stale {
        assert_eq!(utxos.unwrap_err().code(), Code::FailedPrecondition);
    } else {
        assert!(utxos.is_ok());
    }
    // An invalid destination reaches normal validation only after a healthy full pass.
    let error = wallet
        .create_unsigned_transactions(CreateUnsignedTransactionsRequest {
            transaction_description: Some(TransactionDescription::default()),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        if stale {
            Code::FailedPrecondition
        } else {
            Code::InvalidArgument
        }
    );
}

#[tokio::test]
async fn sync_recovery_refuses_stale_cache_and_recovers_after_all_sync_rpcs() {
    let node = Node::default();
    node.failed_operation.store(1, SeqCst);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        grpc_tonic::transport::Server::builder()
            .add_service(
                RpcServer::new(node.clone())
                    .accept_compressed(grpc_tonic::codec::CompressionEncoding::Gzip),
            )
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let client = Arc::new(
        GrpcClient::connect(format!("grpc://{address}"))
            .await
            .unwrap(),
    );
    let directory = tempfile::tempdir().unwrap();
    let keys_file_path = directory
        .path()
        .join("keys.json")
        .to_string_lossy()
        .into_owned();
    kaswallet_create::generate_keys_file::generate_keys_file(
        Arc::new(kaswallet_create::args::Args {
            simnet: true,
            ..Default::default()
        }),
        keys_file_path.clone(),
        Arc::new(vec![create_known_test_mnemonic()]),
        String::new().into(),
        vec![],
    )
    .unwrap();
    let listen = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let daemon = Daemon::new(Arc::new(Args {
        simnet: true,
        keys_file_path: Some(keys_file_path),
        listen: listen.to_string(),
        sync_interval_millis: 100,
        ..Default::default()
    }));
    let (sync_task, wallet_server) = daemon
        .start_with_kaspad_client_and_consensus_params(client.clone(), SIMNET_PARAMS.clone())
        .await
        .unwrap();
    let mut wallet = timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(wallet) = WalletClient::connect(format!("http://{listen}")).await {
                break wallet;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let result = timeout(Duration::from_secs(90), async {
        // Fail each successive initial-pass RPC, without ever publishing fresh data.
        for operation in 1..=3 {
            node.failed_operation.store(operation, SeqCst);
            node.wait_for_failure(operation, 0).await;
            assert_cache_state(&mut wallet, true).await;
            assert!(
                !sync_task.is_finished(),
                "initial RPC failure terminated sync"
            );
        }
        node.failed_operation.store(0, SeqCst);
        assert_cache_state(&mut wallet, false).await;

        for operation in 1..=3 {
            let before = node.failures[operation - 1].load(SeqCst);
            node.failed_operation.store(operation, SeqCst);
            node.wait_for_failure(operation, before).await;
            assert_cache_state(&mut wallet, true).await;
            assert!(
                wallet
                    .get_addresses(GetAddressesRequest::default())
                    .await
                    .is_ok()
            );
            if operation == 3 {
                assert!(
                    wallet
                        .new_address(NewAddressRequest::default())
                        .await
                        .is_ok()
                );
            }
            assert!(
                !sync_task.is_finished(),
                "periodic RPC failure terminated sync"
            );
            node.failed_operation.store(0, SeqCst);
            assert_cache_state(&mut wallet, false).await;
        }
    })
    .await;

    sync_task.abort();
    let _ = sync_task.await;
    wallet_server.abort();
    let _ = wallet_server.await;
    timeout(Duration::from_secs(5), client.disconnect())
        .await
        .unwrap()
        .unwrap();
    server.abort();
    let _ = server.await;
    result.expect("sync recovery scenario timed out");
}
