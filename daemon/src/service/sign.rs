use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletResult;
use common::model::WalletSignableTransaction;
use proto::kaswallet_proto::{SignRequest, SignResponse};
use secrecy::SecretString;

impl KasWalletService {
    pub(crate) async fn sign(&self, request: SignRequest) -> WalletResult<SignResponse> {
        let unsigned_transactions: Vec<WalletSignableTransaction> = request
            .unsigned_transactions
            .into_iter()
            .map(WalletSignableTransaction::try_from)
            .collect::<WalletResult<Vec<_>>>()?;

        // Reject wire-supplied unsigned txs whose subnetwork id does not
        // match the daemon's configured lane. This is the only Sign-side
        // surface that produces signatures, so gating here ensures a
        // lane-bound daemon never signs a cross-lane tx — even if the
        // caller bypasses Send/CreateUnsignedTransactions and submits a
        // hand-built unsigned via Sign + Broadcast.
        for unsigned in &unsigned_transactions {
            self.ensure_subnetwork_id_matches(&unsigned.transaction.inner().tx.subnetwork_id)?;
        }

        // Wrap the password as soon as it crosses the protobuf boundary so it
        // is zeroized on Drop and `Debug`-redacted from any log line.
        let password = SecretString::from(request.password);
        let signed_transactions = self
            .sign_transactions(unsigned_transactions, &password)
            .await?;

        Ok(SignResponse {
            signed_transactions: signed_transactions.into_iter().map(Into::into).collect(),
        })
    }

    // Signing itself lives in `crate::signer` (free functions shared with
    // standalone binaries); this thin method keeps the service call sites
    // (`send.rs`, `sign()` above) unchanged.
    pub(crate) async fn sign_transactions(
        &self,
        unsigned_transactions: Vec<WalletSignableTransaction>,
        password: &SecretString,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        crate::signer::sign_transactions(&self.keys, unsigned_transactions, password)
    }
}
