use crate::utils::ExistingContract;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::str::FromStr;
use tokio::sync::{Mutex, OnceCell};
use ton_block::MsgAddressInt;
use tvm_rpc_client::{ClientOptions, RpcClient};
use url::Url;

use super::*;

#[cfg(feature = "ton")]
const RPC_URL: &str = "https://jrpc-ton.broxus.com";

#[cfg(not(feature = "ton"))]
const RPC_URL: &str = "https://jrpc.everwallet.net/proto";

static RPC_CLIENT: OnceCell<Mutex<RpcClient>> = OnceCell::const_new();

pub async fn get_rpc_client() -> &'static Mutex<RpcClient> {
    RPC_CLIENT
        .get_or_init(|| async {
            let client = RpcClient::new(
                vec![Url::from_str(RPC_URL).unwrap()],
                ClientOptions::default(),
            )
            .await
            .unwrap();
            Mutex::new(client)
        })
        .await
}

async fn get_existing_contract(address: &str) -> ExistingContract {
    let rpc_client = get_rpc_client().await;
    let rpc_client_guard = rpc_client.lock().await;

    let state = rpc_client_guard
        .get_contract_state(&MsgAddressInt::from_str(address).unwrap(), None)
        .await
        .unwrap()
        .unwrap();

    ExistingContract {
        account: state.account,
        last_transaction_id: state.last_transaction_id,
    }
}

#[cfg(not(feature = "ton"))]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeAssetsResponse {
    chain_id_tokens: std::collections::HashMap<u32, Vec<BridgeAssetToken>>,
}

#[cfg(not(feature = "ton"))]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeAssetToken {
    address: String,
}

#[cfg(not(feature = "ton"))]
#[tokio::test]
#[ignore = "requires local config.yaml with token_supply_guard and live Venom RPC"]
async fn venom_token_root_getters_from_config() -> Result<()> {
    let config: crate::config::AppConfig = broxus_util::read_config("config.yaml")?;
    let guard = config
        .bridge_settings
        .token_supply_guard
        .as_ref()
        .context("token_supply_guard must be configured")?;

    let chain_ids = config
        .bridge_settings
        .evm_networks
        .iter()
        .map(|network| network.chain_id)
        .collect::<Vec<_>>();
    let assets = reqwest::Client::new()
        .post(guard.bridge_api_url.join("v1/transfers/tokens_info")?)
        .json(&serde_json::json!({ "chainIds": chain_ids }))
        .send()
        .await?
        .error_for_status()?
        .json::<BridgeAssetsResponse>()
        .await?;
    let rpc_client = RpcClient::new(
        config.bridge_settings.rpc_endpoints,
        ClientOptions::default(),
    )
    .await?;

    let roots = assets
        .chain_id_tokens
        .into_values()
        .flatten()
        .map(|token| token.address)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !roots.is_empty(),
        "Bridge API returned no TVM token mappings for configured EVM networks"
    );

    let mut failures = Vec::new();
    for root in roots {
        let root = MsgAddressInt::from_str(&root).context("invalid TVM root in bridge assets")?;
        let Some(state) = rpc_client.get_contract_state(&root, None).await? else {
            failures.push(format!("{root}: contract is not deployed"));
            continue;
        };
        let contract = ExistingContract {
            account: state.account,
            last_transaction_id: state.last_transaction_id,
        };

        let token_root = TokenRootContract(&contract);
        let total_supply = token_root.total_supply();
        let root_owner = token_root.root_owner();
        match (total_supply, root_owner) {
            (Ok(total_supply), Ok(root_owner)) => {
                tracing::info!(token_root = %root, total_supply, %root_owner, "verified Venom token root getters");
                return Ok(());
            }
            (total_supply, root_owner) if failures.len() < 5 => {
                failures.push(format!(
                    "{root}: totalSupply={:?}, rootOwner={:?}",
                    total_supply.err(),
                    root_owner.err(),
                ));
            }
            _ => {}
        }
    }

    anyhow::bail!(
        "No token root from bridge API supports totalSupply and rootOwner. Attempts: {}",
        failures.join("; "),
    )
}

#[cfg(feature = "ton")]
#[tokio::test]
async fn get_tvm_evm_decoded_data_test() {
    let contract =
        get_existing_contract("0:5616ddb058f9ab1e3ceceed45c40c15f4f8ef6d99f43a6312ff623443c5468f0") // TVM -> EVM native event
            .await;
    let data = TvmEvmEventContract(&contract).event_decoded_data().unwrap();

    assert_eq!(
        data.token.to_string(),
        "0:b113a994b5024a16719f69139328eb759596c38a25f59028b146fecdc3621dfe"
    );
    assert_eq!(data.name, "Tether USD");
    assert_eq!(data.symbol, "USD₮");
    assert_eq!(data.decimals, 6);
}

#[cfg(feature = "ton")]
#[tokio::test]
async fn get_evm_tvm_decoded_data_test() {
    let contract =
        get_existing_contract("0:c9a7fdec418f5f020b20b600d0eb10df9a2eb9762b205a431c79787a26589d57") // EVM -> TVM native event
            .await;
    let data = EvmTvmEventContract(&contract).event_decoded_data().unwrap();

    assert_eq!(
        DisplayAddr(data.token).to_string(),
        "0:b113a994b5024a16719f69139328eb759596c38a25f59028b146fecdc3621dfe" // USDT minter
    );
    assert_eq!(
        data.proxy.to_string(),
        "0:31f98edaf5cd92e674799c0e4bc5cd8e050e4e9401e29f1766f4d27ccc87377d" // Bridge Proxy
    );
    assert_eq!(
        data.token_wallet.to_string(),
        "0:b40d62f8f20e725cf64101c0a933693b2453d94405ee867fdf18f4d6956a29d1" // Bridge Proxy USDT Wallet
    );
}

#[cfg(feature = "ton")]
#[tokio::test]
async fn get_jetton_wallet_address_test() {
    let contract =
        get_existing_contract("0:b113a994b5024a16719f69139328eb759596c38a25f59028b146fecdc3621dfe") // USDT minter
            .await;
    let owner_address = MsgAddressInt::from_str(
        "0:31f98edaf5cd92e674799c0e4bc5cd8e050e4e9401e29f1766f4d27ccc87377d", // Bridge Proxy
    )
    .unwrap();
    let wallet_address = JettonMinterContract(&contract)
        .get_wallet_address(&owner_address)
        .unwrap();

    assert_eq!(
        wallet_address.to_string(),
        "0:b40d62f8f20e725cf64101c0a933693b2453d94405ee867fdf18f4d6956a29d1" // Bridge Proxy USDT Wallet
    );
}

#[cfg(not(feature = "ton"))]
#[tokio::test]
async fn get_evm_tvm_decoded_data_test() {
    let contract =
        get_existing_contract("0:8b176c8b79211250259748842df71776375e4e72996c2b3545b90563820dda4a") // EVM -> TVM native event
            .await;
    let data = EvmTvmEventContract(&contract).event_decoded_data().unwrap();

    assert_eq!(
        DisplayAddr(data.token).to_string(),
        "0:a49cd4e158a9a15555e624759e2e4e766d22600b7800d891e46f9291f044a93d" // USDT token root
    );
    assert_eq!(
        data.proxy.to_string(),
        "0:36122a25a11e8772dc5d94f5f6a653d4661f6e474bc85cb275aece185acd62a4" // Bridge Proxy
    );
    assert_eq!(
        data.token_wallet.to_string(),
        "0:969013414cc804caec5229de43c253d993dca794eca7913fe7d2af4ff52d15f4" // Bridge Proxy USDT Wallet
    );
}

#[cfg(not(feature = "ton"))]
#[tokio::test]
async fn get_wallet_of_test() {
    let contract =
        get_existing_contract("0:a49cd4e158a9a15555e624759e2e4e766d22600b7800d891e46f9291f044a93d") // USDT token root
            .await;
    let owner_address = MsgAddressInt::from_str(
        "0:36122a25a11e8772dc5d94f5f6a653d4661f6e474bc85cb275aece185acd62a4", // Bridge Proxy
    )
    .unwrap();
    let wallet_address = TokenRootContract(&contract)
        .wallet_of(&owner_address)
        .unwrap();

    assert_eq!(
        wallet_address.to_string(),
        "0:969013414cc804caec5229de43c253d993dca794eca7913fe7d2af4ff52d15f4" // Bridge Proxy USDT Wallet
    );
}
