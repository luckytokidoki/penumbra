use std::collections::BTreeMap;

use penumbra_proto::{
    stake::{self as pb},
    Protobuf,
};
use serde::{Deserialize, Serialize};

use crate::{FundingStream, IdentityKey, ValidatorState};

pub type RateDataById = BTreeMap<IdentityKey, RateData>;

/// Describes a validator's reward rate and voting power in some epoch.
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(try_from = "pb::RateData", into = "pb::RateData")]
pub struct RateData {
    /// The validator's identity key.
    pub identity_key: IdentityKey,
    /// The index of the epoch for which this rate is valid.
    pub epoch_index: u64,
    /// The validator-specific reward rate.
    pub validator_reward_rate: u64,
    /// The validator-specific exchange rate.
    pub validator_exchange_rate: u64,
}

impl RateData {
    /// Compute the validator rate data for the epoch following the current one.
    pub fn next(
        &self,
        base_rate_data: &BaseRateData,
        funding_streams: &[FundingStream],
        validator_state: &ValidatorState,
    ) -> RateData {
        let constant_rate =
            // Non-Active validator states result in a constant rate. This means
            // the next epoch's rate is set to the current rate.
            RateData {
                identity_key: self.identity_key.clone(),
                epoch_index: self.epoch_index + 1,
                validator_reward_rate: self.validator_reward_rate,
                validator_exchange_rate: self.validator_exchange_rate,
            };

        match validator_state {
            // if a validator is slashed, their rates are updated to include the slashing penalty
            // and then held constant.
            //
            // if a validator is slashed during the epoch transition the current epoch's rate is set
            // to the slashed value (during end_block) and in here, the next epoch's rate is held constant.
            ValidatorState::Slashed => {
                return constant_rate;
            }
            // if a validator isn't part of the consensus set, we do not update their rates
            ValidatorState::Inactive => {
                return constant_rate;
            }
            ValidatorState::Unbonding { unbonding_epoch: _ } => {
                return constant_rate;
            }
            ValidatorState::Active => {}
        };

        // compute the validator's total commission
        let commission_rate_bps = funding_streams
            .iter()
            .fold(0u64, |total, stream| total + stream.rate_bps as u64);

        if commission_rate_bps > 1_0000 {
            // we should never hit this branch: validator funding streams should be verified not to
            // sum past 100% in the state machine's validation of registration of new funding
            // streams
            panic!("commission rate sums to > 100%")
        }

        // compute next validator reward rate
        // 1 bps = 1e-4, so here we group digits by 4s rather than 3s as is usual
        let validator_reward_rate = ((1_0000_0000u64 - (commission_rate_bps * 1_0000))
            * base_rate_data.base_reward_rate)
            / 1_0000_0000;

        // compute validator exchange rate
        let validator_exchange_rate = (self.validator_exchange_rate
            * (self.validator_reward_rate + 1_0000_0000))
            / 1_0000_0000;

        RateData {
            identity_key: self.identity_key.clone(),
            epoch_index: self.epoch_index + 1,
            validator_reward_rate,
            validator_exchange_rate,
        }
    }

    /// Computes the amount of delegation tokens corresponding to the given amount of unbonded stake.
    ///
    /// # Warning
    ///
    /// Given a pair `(delegation_amount, unbonded_amount)` and `rate_data`, it's possible to have
    /// ```rust,ignore
    /// delegation_amount == rate_data.delegation_amount(unbonded_amount)
    /// ```
    /// or
    /// ```rust,ignore
    /// unbonded_amount == rate_data.unbonded_amount(delegation_amount)
    /// ```
    /// but in general *not both*, because the computation involves rounding.
    pub fn delegation_amount(&self, unbonded_amount: u64) -> u64 {
        // validator_exchange_rate fits in 32 bits, but unbonded_amount is 64-bit;
        // upconvert to u128 intermediates and panic if the result is too large (unlikely)
        ((unbonded_amount as u128 * 1_0000_0000) / self.validator_exchange_rate as u128)
            .try_into()
            .unwrap()
    }

    pub fn slash(&mut self, slashing_penalty: u64) {
        // Slashing penalty is in base points
        self.validator_reward_rate = self
            .validator_reward_rate
            .saturating_sub(self.validator_reward_rate * slashing_penalty / 1_0000_0000);
    }

    /// Computes the amount of unbonded stake corresponding to the given amount of delegation tokens.
    ///
    /// # Warning
    ///
    /// Given a pair `(delegation_amount, unbonded_amount)` and `rate_data`, it's possible to have
    /// ```rust,ignore
    /// delegation_amount == rate_data.delegation_amount(unbonded_amount)
    /// ```
    /// or
    /// ```rust,ignore
    /// unbonded_amount == rate_data.unbonded_amount(delegation_amount)
    /// ```
    /// but in general *not both*, because the computation involves rounding.
    pub fn unbonded_amount(&self, delegation_amount: u64) -> u64 {
        // validator_exchange_rate fits in 32 bits, but unbonded_amount is 64-bit;
        // upconvert to u128 intermediates and panic if the result is too large (unlikely)
        ((delegation_amount as u128 * self.validator_exchange_rate as u128) / 1_0000_0000)
            .try_into()
            .unwrap()
    }

    /// Computes the validator's voting power at this epoch given the total supply of the
    /// validator's delegation tokens.
    pub fn voting_power(&self, total_delegation_tokens: u64, base_rate_data: &BaseRateData) -> u64 {
        ((total_delegation_tokens as u128 * self.validator_exchange_rate as u128)
            / base_rate_data.base_exchange_rate as u128)
            .try_into()
            .unwrap()
    }
}

/// Describes the base reward and exchange rates in some epoch.
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(try_from = "pb::BaseRateData", into = "pb::BaseRateData")]
pub struct BaseRateData {
    /// The index of the epoch for which this rate is valid.
    pub epoch_index: u64,
    /// The base reward rate.
    pub base_reward_rate: u64,
    /// The base exchange rate.
    pub base_exchange_rate: u64,
}

impl BaseRateData {
    /// Compute the base rate data for the epoch following the current one,
    /// given the next epoch's base reward rate.
    pub fn next(&self, base_reward_rate: u64) -> BaseRateData {
        let base_exchange_rate =
            (self.base_exchange_rate * (base_reward_rate + 1_0000_0000)) / 1_0000_0000;
        BaseRateData {
            base_exchange_rate,
            base_reward_rate,
            epoch_index: self.epoch_index + 1,
        }
    }
}

impl Protobuf<pb::RateData> for RateData {}

impl From<RateData> for pb::RateData {
    fn from(v: RateData) -> Self {
        pb::RateData {
            identity_key: Some(v.identity_key.into()),
            epoch_index: v.epoch_index,
            validator_reward_rate: v.validator_reward_rate,
            validator_exchange_rate: v.validator_exchange_rate,
        }
    }
}

impl TryFrom<pb::RateData> for RateData {
    type Error = anyhow::Error;
    fn try_from(v: pb::RateData) -> Result<Self, Self::Error> {
        Ok(RateData {
            identity_key: v
                .identity_key
                .ok_or_else(|| anyhow::anyhow!("missing identity key"))?
                .try_into()?,
            epoch_index: v.epoch_index,
            validator_reward_rate: v.validator_reward_rate,
            validator_exchange_rate: v.validator_exchange_rate,
        })
    }
}

impl Protobuf<pb::BaseRateData> for BaseRateData {}

impl From<BaseRateData> for pb::BaseRateData {
    fn from(v: BaseRateData) -> Self {
        pb::BaseRateData {
            epoch_index: v.epoch_index,
            base_reward_rate: v.base_reward_rate,
            base_exchange_rate: v.base_exchange_rate,
        }
    }
}

impl TryFrom<pb::BaseRateData> for BaseRateData {
    type Error = anyhow::Error;
    fn try_from(v: pb::BaseRateData) -> Result<Self, Self::Error> {
        Ok(BaseRateData {
            epoch_index: v.epoch_index,
            base_reward_rate: v.base_reward_rate,
            base_exchange_rate: v.base_exchange_rate,
        })
    }
}
