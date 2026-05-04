use crate::address_manager::AddressManager;
use crate::sync_manager::SyncManager;
use crate::transaction_generator::TransactionGenerator;
use crate::utxo_manager::UtxoManager;
use common::keys::Keys;
use kaspa_grpc_client::GrpcClient;
use proto::kaswallet_proto::wallet_server::Wallet;
use proto::kaswallet_proto::{
    BroadcastRequest, BroadcastResponse, CreateUnsignedTransactionsRequest,
    CreateUnsignedTransactionsResponse, GetAddressesRequest, GetAddressesResponse,
    GetBalanceRequest, GetBalanceResponse, GetUtxosRequest, GetUtxosResponse, GetVersionRequest,
    GetVersionResponse, NewAddressRequest, NewAddressResponse, SendRequest, SendResponse,
    SignRequest, SignResponse,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::instrument;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_request_id() -> u64 {
    REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub struct KasWalletService {
    pub(crate) kaspa_client: Arc<GrpcClient>,
    pub(crate) keys: Arc<Keys>,
    pub(crate) address_manager: Arc<Mutex<AddressManager>>,
    pub(crate) utxo_manager: Arc<Mutex<UtxoManager>>,
    pub(crate) transaction_generator: Arc<Mutex<TransactionGenerator>>,
    pub(crate) sync_manager: Arc<SyncManager>,
    pub(crate) submit_transaction_mutex: Mutex<()>,
}

impl KasWalletService {
    pub fn new(
        kaspa_client: Arc<GrpcClient>,
        keys: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<Mutex<UtxoManager>>,
        transaction_generator: Arc<Mutex<TransactionGenerator>>,
        sync_manager: Arc<SyncManager>,
    ) -> Self {
        Self {
            kaspa_client,
            keys,
            address_manager,
            utxo_manager,
            transaction_generator,
            sync_manager,
            submit_transaction_mutex: Mutex::new(()),
        }
    }
}

#[tonic::async_trait]
impl Wallet for KasWalletService {
    #[instrument(skip(self, request), fields(request_id = next_request_id()), err(Display))]
    async fn get_addresses(
        &self,
        request: Request<GetAddressesRequest>,
    ) -> Result<Response<GetAddressesResponse>, Status> {
        let addresses = self
            .get_addresses(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(GetAddressesResponse { address: addresses }))
    }

    #[instrument(skip(self, request), fields(request_id = next_request_id()), err(Display))]
    async fn new_address(
        &self,
        request: Request<NewAddressRequest>,
    ) -> Result<Response<NewAddressResponse>, Status> {
        let response = self
            .new_address(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(response))
    }

    #[instrument(skip(self, request), fields(request_id = next_request_id()), err(Display))]
    async fn get_balance(
        &self,
        request: Request<GetBalanceRequest>,
    ) -> Result<Response<GetBalanceResponse>, Status> {
        let response = self
            .get_balance(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(response))
    }

    #[instrument(skip(self, request), fields(request_id = next_request_id()), err(Display))]
    async fn get_utxos(
        &self,
        request: Request<GetUtxosRequest>,
    ) -> Result<Response<GetUtxosResponse>, Status> {
        let response = self
            .get_utxos(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(response))
    }

    #[instrument(skip(self, request), fields(request_id = next_request_id()), err(Display))]
    async fn create_unsigned_transactions(
        &self,
        request: Request<CreateUnsignedTransactionsRequest>,
    ) -> Result<Response<CreateUnsignedTransactionsResponse>, Status> {
        let response = self
            .create_unsigned_transactions(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(response))
    }

    #[instrument(skip(self, request), fields(request_id = next_request_id()), err(Display))]
    async fn sign(&self, request: Request<SignRequest>) -> Result<Response<SignResponse>, Status> {
        let response = self
            .sign(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(response))
    }

    #[instrument(skip(self, request), fields(request_id = next_request_id()), err(Display))]
    async fn broadcast(
        &self,
        request: Request<BroadcastRequest>,
    ) -> Result<Response<BroadcastResponse>, Status> {
        let response = self
            .broadcast(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(response))
    }

    #[instrument(
        skip(self, request),
        fields(
            request_id = next_request_id(),
            amount_sompi = tracing::field::Empty,
        ),
        err(Display)
    )]
    async fn send(&self, request: Request<SendRequest>) -> Result<Response<SendResponse>, Status> {
        // Record amount_sompi only when transaction_description is present, so a
        // missing description does not collapse into the same `amount_sompi = 0`
        // span value as a real zero-amount request.
        if let Some(d) = request.get_ref().transaction_description.as_ref() {
            tracing::Span::current().record("amount_sompi", d.amount);
        }

        let response = self
            .send(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(response))
    }

    #[instrument(skip(self, request), fields(request_id = next_request_id()), err(Display))]
    async fn get_version(
        &self,
        request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        let _ = request;
        Ok(Response::new(GetVersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }
}
