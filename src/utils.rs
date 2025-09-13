use std::cell::Ref;

use anchor_lang::prelude::AccountMeta;
use base64::{Engine, engine::general_purpose::STANDARD as base64};
use rust_decimal::Decimal;
use solana_sdk::{hash::Hash, instruction::Instruction, pubkey::Pubkey};
use solana_system_interface::program;
use spl_associated_token_account::get_associated_token_address;
use switchboard_on_demand::{
    Discriminator, ON_DEMAND_MAINNET_PID, OracleAccountData, PullFeedAccountData, State,
};
use switchboard_on_demand_client::{
    CrossbarClient, FeedConfig, FetchSignaturesConsensusParams, FetchSignaturesConsensusResponse,
    FetchSignaturesParams, Gateway, NATIVE_MINT, OracleResponse, PullFeedSubmitResponse,
    PullFeedSubmitResponseConsensus, PullFeedSubmitResponseConsensusParams,
    PullFeedSubmitResponseParams, SolanaSubmitSignaturesParams, Submission, encode_jobs,
    oracle_job::OracleJob,
    secp256k1::{Secp256k1InstructionUtils, SecpSignature},
};

use crate::app::{AppError, AppResult};

fn build_oracle_accounts(oracles: &[Pubkey]) -> Vec<AccountMeta> {
    oracles
        .iter()
        .flat_map(|oracle| {
            vec![
                AccountMeta::new_readonly(*oracle, false),
                AccountMeta::new(OracleAccountData::stats_key(oracle), false),
            ]
        })
        .collect()
}

pub fn get_solana_submit_signatures_ix(
    slot: u64,
    responses: Vec<OracleResponse>,
    params: SolanaSubmitSignaturesParams,
) -> Instruction {
    let mut remaining_accounts = Vec::new();
    let mut submissions = Vec::new();

    for OracleResponse {
        recovery_id,
        signature,
        value,
        ..
    } in responses.clone().into_iter()
    {
        let mut value_i128 = i128::MAX;

        if let Some(mut val) = value {
            val.rescale(18);
            value_i128 = val.mantissa();
        }

        submissions.push(Submission {
            value: value_i128,
            signature,
            recovery_id,
            offset: 0,
        });
    }

    let oracle_keys: Vec<Pubkey> = responses.iter().map(|resp| resp.oracle).collect();
    remaining_accounts.extend(build_oracle_accounts(&oracle_keys));

    // pull_feed_submit_response ix
    let mut submit_ix = Instruction {
        program_id: ON_DEMAND_MAINNET_PID,
        data: PullFeedSubmitResponseParams { slot, submissions }.data(),
        accounts: PullFeedSubmitResponse {
            feed: params.feed,
            queue: params.queue,
            program_state: State::get_pda(),
            recent_slothashes: solana_sdk::sysvar::slot_hashes::ID,
            payer: params.payer,
            system_program: program::ID,
            reward_vault: get_associated_token_address(&params.queue, &NATIVE_MINT),
            token_program: spl_token::ID,
            token_mint: *NATIVE_MINT,
        }
        .to_account_metas(None),
    };

    submit_ix.accounts.extend(remaining_accounts);

    submit_ix
}

pub async fn get_oracle_submissions(
    feed_data: &PullFeedAccountData,
    gateway: &Gateway,
    recent_blockhash: Hash,
) -> AppResult<Vec<OracleResponse>> {
    let crossbar = CrossbarClient::default();

    let feed_hash = hex::encode(feed_data.feed_hash);

    let jobs_data = crossbar
        .fetch(&feed_hash)
        .await
        .map_err(|error| AppError::ParsingError(format!("{error:#?}")))?;

    let jobs: Vec<OracleJob> = serde_json::from_value(jobs_data.get("jobs").unwrap().clone())?;

    let encoded_jobs = encode_jobs(&jobs);

    let num_signatures = (feed_data.min_sample_size as f64
        + ((feed_data.min_sample_size as f64) / 3.0).ceil()) as u32;

    let price_signatures = gateway
        .fetch_signatures_from_encoded(FetchSignaturesParams {
            recent_hash: Some(recent_blockhash.to_string()),
            encoded_jobs: encoded_jobs.clone(),
            num_signatures,
            max_variance: Some((feed_data.max_variance / 1_000_000_000) as u32),
            min_responses: Some(feed_data.min_responses),
            use_timestamp: Some(false),
        })
        .await
        .map_err(|error| AppError::ParsingError(format!("{error:#?}")))?;

    let oracle_responses: Vec<OracleResponse> = price_signatures
        .responses
        .iter()
        .map(|x| {
            let value = x.success_value.parse::<i128>().ok();
            let mut formatted_value = None;
            if let Some(val) = value {
                formatted_value = Some(Decimal::from_i128_with_scale(val, 18));
            }
            OracleResponse {
                value: formatted_value,
                error: x.failure_error.clone(),
                oracle: Pubkey::new_from_array(
                    hex::decode(x.oracle_pubkey.clone())
                        .unwrap()
                        .try_into()
                        .unwrap(),
                ),
                recovery_id: x.recovery_id as u8,
                signature: base64
                    .decode(x.signature.clone())
                    .unwrap_or_default()
                    .try_into()
                    .unwrap_or([0; 64]),
            }
        })
        .collect();

    Ok(oracle_responses)
}

fn extract_consensus_values(price_signatures: &FetchSignaturesConsensusResponse) -> Vec<i128> {
    price_signatures
        .median_responses
        .iter()
        .map(|mr| mr.value.parse::<i128>().unwrap_or(i128::MAX))
        .collect()
}

fn extract_oracle_keys(price_signatures: &FetchSignaturesConsensusResponse) -> AppResult<Vec<Pubkey>> {
    price_signatures
        .oracle_responses
        .iter()
        .map(|x| {
            let decoded = hex::decode(
                x.feed_responses
                    .first()
                    .ok_or_else(|| AppError::ParsingError("No feed responses found".to_string()))?
                    .oracle_pubkey
                    .clone()
            ).map_err(|e| AppError::ParsingError(format!("Failed to decode oracle pubkey: {e}")))?;

            let array: [u8; 32] = decoded
                .try_into()
                .map_err(|_| AppError::ParsingError("Invalid oracle pubkey length".to_string()))?;

            Ok(Pubkey::new_from_array(array))
        })
        .collect()
}

fn build_secp_signatures(price_signatures: &FetchSignaturesConsensusResponse) -> AppResult<Vec<SecpSignature>> {
    price_signatures
        .oracle_responses
        .iter()
        .map(|oracle_response| {
            let eth_address = hex::decode(&oracle_response.eth_address)
                .map_err(|e| AppError::ParsingError(format!("Invalid eth_address: {e}")))?
                .try_into()
                .map_err(|_| AppError::ParsingError("Invalid eth_address length".to_string()))?;

            let signature = base64
                .decode(&oracle_response.signature)
                .map_err(|e| AppError::ParsingError(format!("Invalid signature: {e}")))?
                .try_into()
                .map_err(|_| AppError::ParsingError("Invalid signature length".to_string()))?;

            let message = base64
                .decode(&oracle_response.checksum)
                .map_err(|e| AppError::ParsingError(format!("Invalid checksum: {e}")))?
                .try_into()
                .map_err(|_| AppError::ParsingError("Invalid checksum length".to_string()))?;

            Ok(SecpSignature {
                eth_address,
                signature,
                message,
                recovery_id: oracle_response.recovery_id as u8,
            })
        })
        .collect()
}

fn build_consensus_instruction_accounts(params: &SolanaSubmitSignaturesParams, oracle: Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(params.feed, false),
        AccountMeta::new_readonly(oracle, false),
        AccountMeta::new(OracleAccountData::stats_key(&oracle), false),
    ]
}

pub fn get_update_consensus_ix(
    params: SolanaSubmitSignaturesParams,
    price_signatures: FetchSignaturesConsensusResponse,
    slot: u64,
) -> AppResult<Vec<Instruction>> {
    let consensus_values = extract_consensus_values(&price_signatures);
    tracing::info!("consensus_ix_data values: {consensus_values:#?}");

    let consensus_ix_data = PullFeedSubmitResponseConsensusParams {
        slot,
        values: consensus_values,
    };

    let oracle_keys = extract_oracle_keys(&price_signatures)?;
    let secp_signatures = build_secp_signatures(&price_signatures)?;

    tracing::info!("secp_signatures (length): {}", secp_signatures.len());

    let instruction_index = 0;
    let secp_ix = Secp256k1InstructionUtils::build_secp256k1_instruction(
        &secp_signatures,
        instruction_index as u8,
    )
    .map_err(|_| {
        AppError::ParsingError(
            "Feed failed to produce signatures: Failed to build secp256k1 instruction".to_string()
        )
    })?;

    let oracle = oracle_keys[instruction_index];
    let remaining_accounts = build_consensus_instruction_accounts(&params, oracle);

    let mut submit_ix = Instruction {
        program_id: ON_DEMAND_MAINNET_PID,
        data: consensus_ix_data.data(),
        accounts: PullFeedSubmitResponseConsensus {
            queue: params.queue,
            program_state: State::get_pda(),
            recent_slothashes: solana_sdk::sysvar::slot_hashes::ID,
            payer: params.payer,
            system_program: program::ID,
            reward_vault: get_associated_token_address(&params.queue, &NATIVE_MINT),
            token_program: spl_token::ID,
            token_mint: *NATIVE_MINT,
        }
        .to_account_metas(None),
    };

    submit_ix.accounts.extend(remaining_accounts);

    Ok(vec![secp_ix, submit_ix])
}

pub async fn get_consensus_signatures(
    feed_data: &PullFeedAccountData,
    gateway: &Gateway,
    recent_blockhash: Hash,
) -> AppResult<FetchSignaturesConsensusResponse> {
    let crossbar = CrossbarClient::default();

    let feed_hash = hex::encode(feed_data.feed_hash);

    let jobs_data = crossbar
        .fetch(&feed_hash)
        .await
        .map_err(|error| AppError::ParsingError(format!("{error:#?}")))?;

    let jobs: Vec<OracleJob> = serde_json::from_value(jobs_data.get("jobs").unwrap().clone())?;

    let encoded_jobs = encode_jobs(&jobs);

    let max_variance = (feed_data.max_variance / 1_000_000_000) as u32;
    let min_responses = feed_data.min_responses;

    let feed_config = FeedConfig {
        encoded_jobs,
        max_variance: Some(max_variance),
        min_responses: Some(min_responses),
    };

    // let num_signatures = feed_data.min_sample_size as u32 + ((feed_data.min_sample_size as f64) / 3.0).ceil() as u32;
    let num_signatures = 1;

    // Call the gateway consensus endpoint and fetch signatures
    let price_signatures = gateway
        .fetch_signatures_consensus(FetchSignaturesConsensusParams {
            recent_hash: Some(recent_blockhash.to_string()),
            num_signatures: Some(num_signatures),
            feed_configs: vec![feed_config],
            use_timestamp: Some(false),
        })
        .await
        .map_err(|error| AppError::ParsingError(format!("{error}")))?;

    Ok(price_signatures)
}

pub fn parse_swb_ignore_alignment(data: Ref<&mut [u8]>) -> AppResult<PullFeedAccountData> {
    if data.len() < 8 {
        return Err(AppError::SwitchboardInvalidAccount);
    }

    if &data[..8] != PullFeedAccountData::DISCRIMINATOR {
        return Err(AppError::SwitchboardInvalidAccount);
    }

    let feed = bytemuck::try_pod_read_unaligned::<PullFeedAccountData>(
        &data[8..8 + std::mem::size_of::<PullFeedAccountData>()],
    )
    .map_err(|_| AppError::SwitchboardInvalidAccount)?;

    Ok(feed)
}

pub enum UrlType {
    SolscanAccount(String),
    SolscanToken(String),
    SolscanTx(String),
}

pub fn construct_url(url_type: UrlType) -> String {
    use UrlType::*;

    let solscan_base_url = "https://solscan.io";

    match url_type {
        SolscanAccount(account_address) => format!("{solscan_base_url}/account/{account_address}"),
        SolscanToken(token_address) => format!("{solscan_base_url}/token/{token_address}"),
        SolscanTx(tx_signature) => format!("{solscan_base_url}/tx/{tx_signature}"),
    }
}
