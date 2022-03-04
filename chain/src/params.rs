use penumbra_crypto::asset;
use penumbra_proto::{chain as pb, crypto as pbc, Protobuf};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct AssetInfo {
    pub asset_id: asset::Id,
    pub denom: asset::Denom,
    pub as_of_block_height: u64,
    pub total_supply: u64,
}

impl Protobuf<pb::AssetInfo> for AssetInfo {}

impl TryFrom<pb::AssetInfo> for AssetInfo {
    type Error = anyhow::Error;

    fn try_from(msg: pb::AssetInfo) -> Result<Self, Self::Error> {
        Ok(AssetInfo {
            asset_id: asset::Id::try_from(msg.asset_id.unwrap())?,
            denom: asset::Denom::try_from(msg.denom.unwrap())?,
            as_of_block_height: msg.as_of_block_height,
            total_supply: msg.total_supply,
        })
    }
}

impl From<AssetInfo> for pb::AssetInfo {
    fn from(ai: AssetInfo) -> Self {
        pb::AssetInfo {
            asset_id: Some(pbc::AssetId::from(ai.asset_id)),
            denom: Some(pbc::Denom::from(ai.denom)),
            as_of_block_height: ai.as_of_block_height,
            total_supply: ai.total_supply,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "pb::ChainParams", into = "pb::ChainParams")]
pub struct ChainParams {
    pub chain_id: String,
    pub epoch_duration: u64,
    pub unbonding_epochs: u64,
    /// The number of validators allowed in the consensus set (Active state).
    pub active_validator_limit: u64,
    /// Slashing penalty in basis points
    pub slashing_penalty: u64,

    /// Whether IBC (forming connections, processing IBC packets) is enabled.
    pub ibc_enabled: bool,
    /// Whether inbound ICS-20 transfers are enabled
    pub inbound_ics20_transfers_enabled: bool,
    /// Whether outbound ICS-20 transfers are enabled
    pub outbound_ics20_transfers_enabled: bool,
}

impl Protobuf<pb::ChainParams> for ChainParams {}

impl From<pb::ChainParams> for ChainParams {
    fn from(msg: pb::ChainParams) -> Self {
        ChainParams {
            chain_id: msg.chain_id,
            epoch_duration: msg.epoch_duration,
            unbonding_epochs: msg.unbonding_epochs,
            active_validator_limit: msg.active_validator_limit,
            slashing_penalty: msg.slashing_penalty,
            ibc_enabled: msg.ibc_enabled,
            inbound_ics20_transfers_enabled: msg.inbound_ics20_transfers_enabled,
            outbound_ics20_transfers_enabled: msg.outbound_ics20_transfers_enabled,
        }
    }
}

impl From<ChainParams> for pb::ChainParams {
    fn from(params: ChainParams) -> Self {
        pb::ChainParams {
            chain_id: params.chain_id,
            epoch_duration: params.epoch_duration,
            unbonding_epochs: params.unbonding_epochs,
            active_validator_limit: params.active_validator_limit,
            slashing_penalty: params.slashing_penalty,
            ibc_enabled: params.ibc_enabled,
            inbound_ics20_transfers_enabled: params.inbound_ics20_transfers_enabled,
            outbound_ics20_transfers_enabled: params.outbound_ics20_transfers_enabled,
        }
    }
}

// TODO: defaults are implemented here as well as in the
// `pd::main`
impl Default for ChainParams {
    fn default() -> Self {
        Self {
            chain_id: String::new(),
            epoch_duration: 8640,
            unbonding_epochs: 30,
            active_validator_limit: 10,
            // 1000 basis points = 10%
            slashing_penalty: 1000,
            ibc_enabled: false,
            inbound_ics20_transfers_enabled: false,
            outbound_ics20_transfers_enabled: false,
        }
    }
}
