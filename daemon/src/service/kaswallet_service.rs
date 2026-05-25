use crate::address_manager::AddressManager;
use crate::sync_manager::SyncManager;
use crate::transaction_generator::TransactionGenerator;
use crate::utxo_manager::UtxoManager;
use common::error_location::ErrorLocation;
use common::errors::{UserInputError, WalletError, WalletResult};
use common::keys::Keys;
use kaspa_consensus_core::subnets::SubnetworkId;
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
use tracing::{instrument, warn};

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
    // Operator-configured lane id. Wire-supplied transactions (Sign,
    // Broadcast) whose `subnetwork_id` does not match this value are
    // rejected at the service boundary so a daemon configured for a
    // specific lane (e.g. IGRA `97b10000…`) cannot be coerced into
    // signing or relaying transactions targeting any other lane.
    pub(crate) configured_subnetwork_id: SubnetworkId,
}

impl KasWalletService {
    pub fn new(
        kaspa_client: Arc<GrpcClient>,
        keys: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<Mutex<UtxoManager>>,
        transaction_generator: Arc<Mutex<TransactionGenerator>>,
        sync_manager: Arc<SyncManager>,
        configured_subnetwork_id: SubnetworkId,
    ) -> Self {
        Self {
            kaspa_client,
            keys,
            address_manager,
            utxo_manager,
            transaction_generator,
            sync_manager,
            submit_transaction_mutex: Mutex::new(()),
            configured_subnetwork_id,
        }
    }

    /// Reject a wire-supplied transaction whose subnetwork id does not match
    /// the daemon's configured lane. Returns `Ok(())` when the daemon is in
    /// native mode (no lane restriction) or the ids match.
    pub(crate) fn ensure_subnetwork_id_matches(
        &self,
        tx_subnetwork_id: &SubnetworkId,
    ) -> WalletResult<()> {
        ensure_subnetwork_id_matches(&self.configured_subnetwork_id, tx_subnetwork_id)
    }
}

/// Pure lane-gate predicate, extracted for unit testing without having to
/// stand up an `Arc<GrpcClient>` and the rest of the service graph.
///
/// We intentionally allow any tx through a native-configured daemon
/// (`configured.is_native()`), so the generic kaspa wallet behavior is
/// preserved — only lane-bound daemons gate by subnetwork id.
pub(crate) fn ensure_subnetwork_id_matches(
    configured: &SubnetworkId,
    tx_subnetwork_id: &SubnetworkId,
) -> WalletResult<()> {
    if configured.is_native() || tx_subnetwork_id == configured {
        return Ok(());
    }
    // Surface lane-mismatch attempts: silent rejections leave on-call with
    // no signal that a probe or misroute happened. Emitted at warn so
    // log aggregators flag it without paging.
    warn!(
        configured = %configured,
        attempted = %tx_subnetwork_id,
        "rejecting wire-supplied tx with mismatched subnetwork id"
    );
    Err(WalletError::from(UserInputError::InvalidArgument {
        reason: format!(
            "transaction subnetwork_id {tx_subnetwork_id} does not match daemon's \
             configured subnetwork_id {configured}",
        ),
        location: ErrorLocation::capture(),
    }))
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

    #[instrument(
        skip(self, request),
        fields(
            request_id = next_request_id(),
            subnetwork_id = %self.configured_subnetwork_id,
        ),
        err(Display)
    )]
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

    #[instrument(
        skip(self, request),
        fields(
            request_id = next_request_id(),
            subnetwork_id = %self.configured_subnetwork_id,
        ),
        err(Display)
    )]
    async fn sign(&self, request: Request<SignRequest>) -> Result<Response<SignResponse>, Status> {
        let response = self
            .sign(request.into_inner())
            .await
            .map_err(Status::from)?;

        Ok(Response::new(response))
    }

    #[instrument(
        skip(self, request),
        fields(
            request_id = next_request_id(),
            subnetwork_id = %self.configured_subnetwork_id,
        ),
        err(Display)
    )]
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
            subnetwork_id = %self.configured_subnetwork_id,
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

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::subnets::{SUBNETWORK_ID_NATIVE, SubnetworkId};

    fn igra_lane() -> SubnetworkId {
        let mut bytes = [0u8; 20];
        bytes[0] = 0x97;
        bytes[1] = 0xb1;
        SubnetworkId::from_bytes(bytes)
    }

    fn other_lane() -> SubnetworkId {
        let mut bytes = [0u8; 20];
        bytes[0] = 0xaa;
        bytes[1] = 0xbb;
        SubnetworkId::from_bytes(bytes)
    }

    #[test]
    fn native_configured_allows_any_tx() {
        ensure_subnetwork_id_matches(&SUBNETWORK_ID_NATIVE, &SUBNETWORK_ID_NATIVE).unwrap();
        ensure_subnetwork_id_matches(&SUBNETWORK_ID_NATIVE, &igra_lane()).unwrap();
        ensure_subnetwork_id_matches(&SUBNETWORK_ID_NATIVE, &other_lane()).unwrap();
    }

    #[test]
    fn matching_lane_passes() {
        ensure_subnetwork_id_matches(&igra_lane(), &igra_lane()).unwrap();
    }

    #[test]
    fn mismatched_lane_is_rejected() {
        let err = ensure_subnetwork_id_matches(&igra_lane(), &other_lane()).unwrap_err();
        let msg = err.to_string();
        // The message must name both ids so on-call can see which lane the
        // tx tried to use vs. which the daemon expected.
        assert!(msg.contains(&igra_lane().to_string()), "got: {msg}");
        assert!(msg.contains(&other_lane().to_string()), "got: {msg}");
    }

    #[test]
    fn lane_daemon_rejects_native_tx() {
        // Critical: a daemon configured for a non-native lane MUST NOT
        // sign or broadcast a native (subnetwork=0×20) transaction.
        let err = ensure_subnetwork_id_matches(&igra_lane(), &SUBNETWORK_ID_NATIVE).unwrap_err();
        assert!(err.to_string().contains("does not match"), "got: {err}");
    }
}
