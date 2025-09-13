mod app;
mod swb;
mod utils;

use app::AppClient;
use dotenv::dotenv;
use solana_sdk::pubkey::Pubkey;
use std::{env, sync::Arc};
use tracing_subscriber::FmtSubscriber;
use switchboard_on_demand_client::FetchUpdateManyParams;
use switchboard_on_demand_client::PullFeed;
use switchboard_on_demand_client::SbContext;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcSimulateTransactionConfig};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signer::keypair::Keypair;
use solana_sdk::signature::EncodableKey;
use solana_sdk::signature::Signer;
use solana_sdk::transaction::{VersionedTransaction};
use solana_sdk::message::{Message, VersionedMessage};
use switchboard_on_demand_client::CrossbarClient;
use switchboard_on_demand_client::QueueAccountData;

pub const SWITCHBOARD_ACCOUNT_QUEUE: Pubkey =
    Pubkey::from_str_const("A43DyUGA7s8eXPxqEjJY6EBu1KKbNgfxF8h17VAHn13w");

#[tokio::main]
async fn main() {
    tracing::info!("lfg🚀🚀");
    let _ = dotenv().ok();
    let private_key = "/path/to/your/solana/id.json";
    let kp = Keypair::read_from_file(&private_key).unwrap();
    let rpc_url =
        env::var("RPC_URL").expect("Missing 'SOLANA_HTTP_URL' in environment variables");

    let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
        // will be written to stdout.
        .with_max_level(tracing::Level::INFO)
        // completes the builder.
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let app_client = Arc::new(AppClient::new(&private_key, rpc_url.clone()));

    let ctx = SbContext::new();
    let rpc_client = Arc::new(RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed()));
    let crossbar = CrossbarClient::new("https://crossbar.switchboard.xyz", true);

    let queue_account_data = QueueAccountData::load(&rpc_client, &SWITCHBOARD_ACCOUNT_QUEUE).await.unwrap();
    let gw = queue_account_data.fetch_gateway_from_crossbar(&crossbar).await.unwrap();
    let (instructions, lookup_tables) = PullFeed::fetch_update_consensus_ix(
        ctx,
        &rpc_client,
        FetchUpdateManyParams {
            crossbar: Some(crossbar),
            debug: Some(true),
            feeds: vec![Pubkey::from_str_const("EUQQD2fNN7h7su5TbWpUnf22zeGtF3RjEX2hgX2YPfLd")],
            gateway: gw,
            num_signatures: Some(1),
            payer: kp.pubkey(),
        },
    ).await.unwrap();

    let recent_blockhash = rpc_client.get_latest_blockhash().await.unwrap();
    let mut message = Message::new(&instructions, Some(&kp.pubkey()));
    message.recent_blockhash = recent_blockhash;
    let versioned_message = VersionedMessage::Legacy(message);
    let versioned_tx = VersionedTransaction::try_new(versioned_message, &[&kp]).unwrap();
    let sim_config = RpcSimulateTransactionConfig {
        sig_verify: false,
        commitment: Some(CommitmentConfig::processed()),
        ..Default::default()
    };
    let sim_res = rpc_client.simulate_transaction_with_config(&versioned_tx, sim_config).await.unwrap();
    println!("sim res: {:?}", sim_res);
}
