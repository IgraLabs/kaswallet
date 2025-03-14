use crate::address_manager::AddressManager;
use crate::args::Args;
use crate::model::{Keychain, WalletAddress};
use crate::sync_manager::SyncManager;
use common::keys::Keys;
use kaspa_wrpc_client::KaspaRpcClient;
use log::trace;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use wallet_proto::wallet_proto::wallet_server::Wallet;
use wallet_proto::wallet_proto::{
    BroadcastRequest, BroadcastResponse, CreateUnsignedTransactionsRequest,
    CreateUnsignedTransactionsResponse, GetAddressesRequest, GetAddressesResponse,
    GetBalanceRequest, GetBalanceResponse, GetUtxosRequest, GetUtxosResponse, GetVersionRequest,
    GetVersionResponse, NewAddressRequest, NewAddressResponse, SendRequest, SendResponse,
    SignRequest, SignResponse,
};

#[derive(Debug)]
pub struct KasWalletService {
    args: Args,
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    keys: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    sync_manager: Arc<Mutex<SyncManager>>,
}

impl KasWalletService {
    pub fn new(
        args: Args,
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        address_manager: Arc<Mutex<AddressManager>>,
        sync_manager: Arc<Mutex<SyncManager>>,
        keys: Arc<Keys>,
    ) -> Self {
        Self {
            args,
            kaspa_rpc_client,
            address_manager,
            sync_manager,
            keys,
        }
    }
}

impl KasWalletService {
    async fn check_is_synced(&self) -> Result<(), Status> {
        let sync_manager = self.sync_manager.lock().await;
        if !sync_manager.is_synced().await {
            return Err(Status::failed_precondition(
                "Wallet is not synced yet. Please wait for the sync to complete.",
            ));
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl Wallet for KasWalletService {
    async fn get_addresses(
        &self,
        request: Request<GetAddressesRequest>,
    ) -> Result<Response<GetAddressesResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());

        self.check_is_synced().await?;

        let mut addresses = vec![];
        let address_manager = self.address_manager.lock().await;
        for i in 0..self.keys.last_used_external_index.load(Relaxed) {
            let wallet_address = WalletAddress {
                index: i,
                cosigner_index: self.keys.cosigner_index,
                keychain: Keychain::External,
            };
            match address_manager.calculate_address(&wallet_address) {
                Ok(address) => addresses.push(address.to_string()),
                Err(e) => {
                    return Err(Status::internal(format!(
                        "Failed to calculate address: {}",
                        e
                    )))
                }
            }
        }

        Ok(Response::new(GetAddressesResponse { address: addresses }))
    }

    async fn new_address(
        &self,
        request: Request<NewAddressRequest>,
    ) -> Result<Response<NewAddressResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());

        self.check_is_synced().await?;

        let address_manager = self.address_manager.lock().await;

        match address_manager.new_address().await {
            Ok((address, _)) => {
                let response = NewAddressResponse { address };
                Ok(Response::new(response))
            }
            Err(e) => Err(Status::internal(format!(
                "Failed to generate new address: {}",
                e
            ))),
        }
    }

    async fn get_balance(
        &self,
        request: Request<GetBalanceRequest>,
    ) -> Result<Response<GetBalanceResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn get_utxos(
        &self,
        request: Request<GetUtxosRequest>,
    ) -> Result<Response<GetUtxosResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn create_unsigned_transactions(
        &self,
        request: Request<CreateUnsignedTransactionsRequest>,
    ) -> Result<Response<CreateUnsignedTransactionsResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn sign(&self, request: Request<SignRequest>) -> Result<Response<SignResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn broadcast(
        &self,
        request: Request<BroadcastRequest>,
    ) -> Result<Response<BroadcastResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn send(&self, request: Request<SendRequest>) -> Result<Response<SendResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn get_version(
        &self,
        request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());

        Ok(Response::new(GetVersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }
}
