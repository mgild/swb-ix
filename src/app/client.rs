use std::{sync::Arc, time::Duration};

use anchor_lang::prelude::Pubkey;
use futures::{stream, StreamExt};
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    nonblocking::rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::RpcFilterType,
    rpc_request::TokenAccountsFilter,
};
use solana_sdk::{
    account::Account, commitment_config::CommitmentConfig, hash::Hash,
    instruction::Instruction,
    message::{v0, AddressLookupTableAccount, Message, VersionedMessage},
    signature::{Keypair, Signature}, signer::Signer, transaction::{VersionedTransaction}
};
use tokio::{sync::Semaphore, time::interval};
use solana_sdk::signature::EncodableKey;
use crate::app::AppError;

use super::error::AppResult;

pub struct AppClient {
    keypair: Arc<Keypair>,
    keypair_pubkey: Pubkey,
    rpc_client: RpcClient,
    rpc_url: String,
    semaphore: Arc<Semaphore>,
}
use solana_client::rpc_response::RpcSimulateTransactionResult;
use solana_client::rpc_response::Response;
impl AppClient {
    pub async fn call_instructions(
        &self,
        alts: Option<&[AddressLookupTableAccount]>,
        instructions: &[Instruction],
        recent_blockhash: Hash,
        signing_keypairs: Option<&[&Keypair]>,
    ) -> AppResult<Response<RpcSimulateTransactionResult>> {
        tracing::info!("call_instructions: {instructions:#?}");

        let default_signing_keypairs: &[&Keypair] = &[&self.keypair];
        let signing_keypairs = signing_keypairs.unwrap_or(default_signing_keypairs);

        let transaction = if alts.is_none() {
            let message = Message::new_with_blockhash(
                instructions,
                Some(&self.keypair_pubkey),
                &recent_blockhash,
            );
            let v0_message = VersionedMessage::Legacy(message);

            VersionedTransaction::try_new(v0_message, signing_keypairs)?
        } else {
            let alts = alts.unwrap();
            let message = v0::Message::try_compile(
                &self.keypair_pubkey,
                instructions,
                alts,
                recent_blockhash,
            )?;
            let v0_message = VersionedMessage::V0(message);

            VersionedTransaction::try_new(v0_message, signing_keypairs)?
        };

        let serialized_size = serde_json::to_vec(&transaction)?.len();
        let size_of_val = size_of_val(&transaction);

        tracing::info!("VersionedTransaction: {transaction:#?}\nserialized_size: {serialized_size} size_of_val: {size_of_val}");

        // if serialized_size > 1232 {
        //     return Err(AppError::TransactionTooLarge(serialized_size));
        // }

        let sim = self
            .rpc_client
            .simulate_transaction(&transaction)
            .await?;
        Ok(sim)
    }

    // ~~~~ keypair related functions ~~~~

    pub fn keypair_pubkey(&self) -> Pubkey {
        self.keypair_pubkey.clone()
    }

    pub fn signing_keypair(&self) -> &Keypair {
        &self.keypair
    }

    pub fn new(private_key: &str, url: String) -> Self {
        let commitment_config = CommitmentConfig::confirmed();

        let keypair = Arc::new(Keypair::read_from_file(private_key).expect("Failed to read keypair file"));

        let keypair_pubkey = keypair.pubkey();
        tracing::info!("Connected wallet - {keypair_pubkey}");

        // request per second rate
        let rqs_rate = 15;
        let semaphore = Arc::new(Semaphore::new(rqs_rate));
        // timeout after 3mins
        let timeout = Duration::from_secs(180);

        let mut interval = interval(Duration::from_secs(15));

        // request per minute handler
        let rps_handler_semaphore = semaphore.clone();
        let _rps_handler = tokio::spawn(async move {
            loop {
                interval.tick().await;

                let available_permits = rps_handler_semaphore.available_permits();

                let to_add = if available_permits < rqs_rate {
                    rqs_rate - available_permits
                } else {
                    0
                };

                // Replenish up to rate.
                if to_add > 0 {
                    rps_handler_semaphore.add_permits(to_add);
                }
            }
        });

        Self {
            keypair,
            keypair_pubkey,
            rpc_client: RpcClient::new_with_timeout_and_commitment(
                url.clone(),
                timeout,
                commitment_config,
            ),
            rpc_url: url,
            semaphore,
        }
    }

    pub async fn get_account(&self, account_pubkey: &Pubkey) -> AppResult<Account> {
        let _permit = self.semaphore.acquire().await?;
        let account = self.rpc_client.get_account(account_pubkey).await?;

        Ok(account)
    }

    pub async fn get_latest_blockhash(&self) -> AppResult<Hash> {
        let _permit = self.semaphore.acquire().await?;
        let latest_hash = self.rpc_client.get_latest_blockhash().await?;

        Ok(latest_hash)
    }

    pub async fn get_multiple_accounts(
        &self,
        accounts_pubkey: &[Pubkey],
        limit: Option<usize>,
    ) -> AppResult<Vec<Option<Account>>> {
        if accounts_pubkey.len() == 0 {
            return Ok(vec![]);
        }

        let _permit = self.semaphore.acquire().await?;

        const CHUNK_SIZE: usize = 5;

        let (chunked_accounts_pubkey, remainder) =
            accounts_pubkey.as_chunks::<CHUNK_SIZE>();
        let mut chunked_accounts_pubkey: Vec<Vec<Pubkey>> = chunked_accounts_pubkey
            .iter()
            .map(|pubkeys| pubkeys.to_vec())
            .collect();

        chunked_accounts_pubkey.push(remainder.to_vec());

        let multiple_accounts = stream::iter(chunked_accounts_pubkey).map(async |accounts_pubkey| {
            match self.rpc_client.get_multiple_accounts(accounts_pubkey.as_slice()).await {
                Err(app_error) => {
                    tracing::error!(
                        "Failed to get multiple accounts with chunk size - {CHUNK_SIZE}\n{app_error:#?}"
                    );

                    let length = accounts_pubkey.len();
                    let default = (0..length).into_iter().map(|_| None).collect::<Vec<Option<Account>>>();

                    default.to_vec()
                }
                Ok(accounts) => accounts
            }
        }).buffer_unordered(limit.unwrap_or(5)).collect::<Vec<_>>().await;

        let accounts = multiple_accounts.into_iter().flatten().collect::<Vec<_>>();

        Ok(accounts)
    }

    pub async fn get_slot(&self) -> AppResult<u64> {
        let _permit = self.semaphore.acquire().await?;
        let slot = self.rpc_client.get_slot().await?;

        Ok(slot)
    }

    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc_client
    }
}
