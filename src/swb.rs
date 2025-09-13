use std::{cell::RefCell, sync::Arc};

use solana_sdk::pubkey::Pubkey;
use switchboard_on_demand::{OracleAccountData, PullFeedAccountData};
use switchboard_on_demand_client::{Gateway, QueueAccountData, SolanaSubmitSignaturesParams};

use crate::{
    SWITCHBOARD_ACCOUNT_QUEUE,
    app::AppClient,
    utils::{
        UrlType, construct_url, get_consensus_signatures, get_oracle_submissions,
        get_solana_submit_signatures_ix, get_update_consensus_ix, parse_swb_ignore_alignment,
    },
};

pub async fn execute_pull_feed_submit_consensus_response(app_client: Arc<AppClient>) {
    let feed_pubkey = Pubkey::from_str_const("6CyMpkE6kb1MkcxhNH5PM7wAPwm2Agu2P4Qa51nQgWfi");

    let feed_account = match app_client.get_account(&feed_pubkey).await {
        Err(app_error) => {
            tracing::error!(
                "Failed to retrieve PullFeedAccountData for - {feed_pubkey}\n{app_error:#?}"
            );
            return;
        }
        Ok(account) => account,
    };

    let mut mut_account_data = feed_account.data.clone();
    let swb_feed_data = RefCell::new(&mut mut_account_data[..]);
    let swb_feed_data = swb_feed_data.borrow();
    let pull_feed_account_data = match parse_swb_ignore_alignment(swb_feed_data) {
        Err(on_demand_error) => {
            tracing::error!(
                "Failed to parse swb_feed_data to PullFeedAccountData for SWB-on-Demand - {feed_pubkey}\n{on_demand_error:#?}"
            );
            return;
        }
        Ok(pull_feed_account_data) => pull_feed_account_data,
    };

    // pull_feed_account_data.f

    let feed_data = &pull_feed_account_data;

    tracing::info!(
        "Successfully deserialized - {feed_pubkey} to PullFeedAccountData - {pull_feed_account_data:#?}"
    );

    let queue_account_data =
        match QueueAccountData::load(app_client.rpc_client(), &SWITCHBOARD_ACCOUNT_QUEUE).await {
            Err(error) => {
                tracing::error!(
                    "Failed to retrieve QueueAccountData - {SWITCHBOARD_ACCOUNT_QUEUE}\n{error:#?}"
                );
                return;
            }
            Ok(data) => data,
        };

    let queue_oracle_keys = queue_account_data.oracle_keys();

    let oracle_accounts = match app_client
        .get_multiple_accounts(&queue_oracle_keys, None)
        .await
    {
        Err(app_error) => {
            tracing::error!(
                "Failed to get multiple accounts - {queue_oracle_keys:#?}\n{app_error:#?}"
            );
            return;
        }
        Ok(accounts) => accounts,
    };

    // gather all the gateway uris the retrieved oracle_accounts contain
    let queue_gateways = oracle_accounts
        .iter()
        .zip(queue_oracle_keys)
        .into_iter()
        .filter_map(|(account, oracle_pubkey)| {
            let Some(oracle_account) = account else {
                tracing::warn!("getMultipleAccounts returned None for - oracle_pubkey: {oracle_pubkey}");
                return None;
            };

            let bytes_data = &oracle_account.data[8..];
            let oracle_account_data: &OracleAccountData =
                bytemuck::try_from_bytes(bytes_data).unwrap();

            let gateway_uri = oracle_account_data.gateway_uri();
            tracing::info!("Successfully deserialized - {oracle_pubkey}\n{oracle_account_data:#?} with gateway - {gateway_uri:#?}");

            let Some(gateway_uri) = gateway_uri else {
                return None;
            };

            Some(Gateway::new(gateway_uri))
        })
        .collect::<Vec<_>>();

    tracing::info!("Constructed queue_gateways => {queue_gateways:#?}");

    let mut retry = 0;
    let max_retry = queue_gateways.len();

    let (latest_blockhash_result, recent_slot_result) =
        tokio::join!(app_client.get_latest_blockhash(), app_client.get_slot());

    let latest_blockhash = match latest_blockhash_result {
        Err(app_error) => {
            tracing::error!("Failed to retrieve latest blockhash\n{app_error:#?}");
            return;
        }
        Ok(blockhash) => blockhash,
    };

    let recent_slot = match recent_slot_result {
        Err(app_error) => {
            tracing::error!("Failed to retrieve current slot\n{app_error:#?}");
            return;
        }
        Ok(slot) => slot,
    };

    let price_signatures;

    loop {
        let gateway = &queue_gateways[retry];

        let function_params_as_string = format!(
            "feed_data: {feed_data:#?} gateway: {gateway:#?} latest_blockhash: {latest_blockhash}"
        );
        match get_consensus_signatures(feed_data, gateway, latest_blockhash).await {
            Err(app_error) => {
                tracing::warn!("Failed to retrieve consensus_signatures\n{app_error:#?}");

                retry += 1;

                if retry < max_retry {
                    tracing::warn!(
                        "Retrying to get consensus signatures after {retry}/{max_retry} tries",
                    );
                    continue;
                }
                tracing::error!("Failed to retrieve consensus_signatures\n{app_error:#?}.");

                return;
            }
            Ok(consensus_response) => {
                tracing::info!(
                    "get_consensus_signatures() from {function_params_as_string} => {consensus_response:#?}"
                );
                price_signatures = consensus_response;
                break;
            }
        };
    }

    let params = SolanaSubmitSignaturesParams {
        feed: feed_pubkey,
        payer: app_client.keypair_pubkey(),
        queue: SWITCHBOARD_ACCOUNT_QUEUE,
    };
    let instructions = match get_update_consensus_ix(params, price_signatures, recent_slot) {
        Err(app_error) => {
            tracing::error!("Failed to construct pull_feed_submit_consensus ix\n{app_error:#?}");
            return;
        }
        Ok(ixs) => ixs,
    };

    let sim = match app_client
        .call_instructions(
            None,
            &instructions,
            //[instructions[0].clone()],
            latest_blockhash,
            None,
        )
        .await
    {
        Err(app_error) => {
            tracing::error!("Failed to execute pull_feed_submit_consensus ix\n{app_error:#?}");
            return;
        }
        Ok(tx) => tx,
    };

    // let tx_url = construct_url(UrlType::SolscanTx(tx_signature.to_string()));

    tracing::info!("Simulation result: {sim:#?}");
    tracing::info!("🎉🎉 Successfully executed pull_feed_submit_consensus ix.");
}

pub async fn execute_pull_feed_submit_response(app_client: Arc<AppClient>) {
    let feed_pubkey = Pubkey::from_str_const("6CyMpkE6kb1MkcxhNH5PM7wAPwm2Agu2P4Qa51nQgWfi");

    let feed_account = match app_client.get_account(&feed_pubkey).await {
        Err(app_error) => {
            tracing::error!(
                "Failed to retrieve PullFeedAccountData for - {feed_pubkey}\n{app_error:#?}"
            );
            return;
        }
        Ok(account) => account,
    };

    let mut mut_account_data = feed_account.data.clone();
    let swb_feed_data = RefCell::new(&mut mut_account_data[..]);
    let swb_feed_data = swb_feed_data.borrow();
    let pull_feed_account_data = match parse_swb_ignore_alignment(swb_feed_data) {
        Err(on_demand_error) => {
            tracing::error!(
                "Failed to parse swb_feed_data to PullFeedAccountData for SWB-on-Demand - {feed_pubkey}\n{on_demand_error:#?}"
            );
            return;
        }
        Ok(pull_feed_account_data) => pull_feed_account_data,
    };

    // pull_feed_account_data.f

    let feed_data = &pull_feed_account_data;

    tracing::info!(
        "Successfully deserialized - {feed_pubkey} to PullFeedAccountData - {pull_feed_account_data:#?}"
    );

    let queue_account_data =
        match QueueAccountData::load(app_client.rpc_client(), &SWITCHBOARD_ACCOUNT_QUEUE).await {
            Err(error) => {
                tracing::error!(
                    "Failed to retrieve QueueAccountData - {SWITCHBOARD_ACCOUNT_QUEUE}\n{error:#?}"
                );
                return;
            }
            Ok(data) => data,
        };

    let queue_oracle_keys = queue_account_data.oracle_keys();

    let oracle_accounts = match app_client
        .get_multiple_accounts(&queue_oracle_keys, None)
        .await
    {
        Err(app_error) => {
            tracing::error!(
                "Failed to get multiple accounts - {queue_oracle_keys:#?}\n{app_error:#?}"
            );
            return;
        }
        Ok(accounts) => accounts,
    };

    // gather all the gateway uris the retrieved oracle_accounts contain
    let queue_gateways = oracle_accounts
        .iter()
        .zip(queue_oracle_keys)
        .into_iter()
        .filter_map(|(account, oracle_pubkey)| {
            let Some(oracle_account) = account else {
                tracing::warn!("getMultipleAccounts returned None for - oracle_pubkey: {oracle_pubkey}");
                return None;
            };

            let bytes_data = &oracle_account.data[8..];
            let oracle_account_data: &OracleAccountData =
                bytemuck::try_from_bytes(bytes_data).unwrap();

            let gateway_uri = oracle_account_data.gateway_uri();
            tracing::info!("Successfully deserialized - {oracle_pubkey}\n{oracle_account_data:#?} with gateway - {gateway_uri:#?}");

            let Some(gateway_uri) = gateway_uri else {
                return None;
            };

            Some(Gateway::new(gateway_uri))
        })
        .collect::<Vec<_>>();

    tracing::info!("Constructed queue_gateways => {queue_gateways:#?}");

    let mut retry = 0;
    let max_retry = queue_gateways.len();

    let (latest_blockhash_result, recent_slot_result) =
        tokio::join!(app_client.get_latest_blockhash(), app_client.get_slot());

    let latest_blockhash = match latest_blockhash_result {
        Err(app_error) => {
            tracing::error!("Failed to retrieve latest blockhash\n{app_error:#?}");
            return;
        }
        Ok(blockhash) => blockhash,
    };

    let recent_slot = match recent_slot_result {
        Err(app_error) => {
            tracing::error!("Failed to retrieve current slot\n{app_error:#?}");
            return;
        }
        Ok(slot) => slot,
    };

    let oracle_responses;

    loop {
        let gateway = &queue_gateways[retry + 8];

        tracing::info!("#{retry} attempt using - {gateway:#?}");

        match get_oracle_submissions(feed_data, gateway, latest_blockhash).await {
            Err(app_error) => {
                tracing::warn!("Failed to retrieve oracle_submissions\n{app_error:#?}");

                retry += 1;

                if retry < max_retry {
                    tracing::warn!(
                        "Retrying to get oracle submissions after {retry}/{max_retry} tries",
                    );
                    continue;
                }
                tracing::error!("Failed to retrieve oracle_submissions\n{app_error:#?}.");

                return;
            }
            Ok(response) => {
                tracing::info!(
                    "Retrieved oracle_responses for - feed_pubkey: {feed_pubkey}\n{response:#?}"
                );
                oracle_responses = response;
                break;
            }
        };
    }

    let params = SolanaSubmitSignaturesParams {
        queue: SWITCHBOARD_ACCOUNT_QUEUE,
        feed: feed_pubkey,
        payer: app_client.keypair_pubkey(),
    };
    let pull_feed_submit_response_ix =
        get_solana_submit_signatures_ix(recent_slot, oracle_responses, params);

    let sim = match app_client
        .call_instructions(
            None,
            &[pull_feed_submit_response_ix],
            //[instructions[0].clone()],
            latest_blockhash,
            None,
        )
        .await
    {
        Err(app_error) => {
            tracing::error!("Failed to execute pull_feed_submit ix\n{app_error:#?}");
            return;
        }
        Ok(tx) => tx,
    };
    tracing::info!("Simulation result: {sim:#?}");


    tracing::info!("🎉🎉 Successfully executed pull_feed_submit ix.");
}
