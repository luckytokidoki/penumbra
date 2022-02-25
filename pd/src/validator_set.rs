use std::{
    borrow::{Borrow, BorrowMut},
    collections::BTreeMap,
};

use anyhow::Result;

use futures::Future;
use penumbra_crypto::{
    asset::{self, Denom, Id},
    Address,
};
use tendermint::{abci::types::ValidatorUpdate, PublicKey};

use crate::state::Reader;
use penumbra_stake::{
    BaseRateData, Epoch, IdentityKey, RateData, Validator, ValidatorInfo, ValidatorState,
    ValidatorStatus, VerifiedValidatorDefinition, STAKING_TOKEN_ASSET_ID, STAKING_TOKEN_DENOM,
};

#[derive(Debug, Clone)]
/// Records the complete state of all validators throughout a block,
/// and is responsible for producing the necessary database queries
/// for persistence.
///
/// After calling `ValidatorSet.commit`, the block is advanced.
/// Internal tracking of validator changes is reset for the new block.
pub struct ValidatorSet {
    /// Records complete validator states as they change during the course of the block.
    ///
    /// Updated as changes occur during the block, but will not be persisted
    /// to the database until the block is committed.
    validator_set: BTreeMap<IdentityKey, ValidatorInfo>,
    /// Database reader.
    reader: Reader,
    /// Validator definitions added during the block. Since multiple definitions could
    /// come in for the same validator during a block, we need to deterministically pick
    /// one definition to use.
    ///
    /// If the definition is for an existing validator, this will be pushed to `self.updated_validators`
    /// during end_block.
    ///
    /// Otherwise if the definition is for a new validator, this will be pushed to `self.new_validators`
    /// during end_block.
    validator_definitions: BTreeMap<IdentityKey, Vec<VerifiedValidatorDefinition>>,
    /// New validators added during the block. Saved and available for staking when the block is committed.
    pub new_validators: Vec<ValidatorInfo>,
    /// Existing validators updated during the block. Saved when the block is committed.
    pub updated_validators: Vec<ValidatorInfo>,
    /// Validators slashed during this block. Saved when the block is committed.
    ///
    /// The validator's rate will have a slashing penalty immediately applied during the current epoch.
    /// Their future rates will be held constant.
    pub slashed_validators: Vec<IdentityKey>,
    /// The net delegations performed in this block per validator.
    pub delegation_changes: BTreeMap<IdentityKey, i64>,
    /// Indicates the epoch the block belongs to.
    pub epoch: Option<Epoch>,
    /// If this is the last block of an epoch, base rates for the next epoch go here.
    pub next_base_rate: Option<BaseRateData>,
    /// If this is the last block of an epoch, validator rates for the next epoch go here.
    pub next_rates: Option<Vec<RateData>>,
    /// The list of updated validator identity keys and powers to send to Tendermint during `end_block`.
    ///
    /// Set in `end_block` and reset to `None` when `tm_validator_updates` is called.
    pub tm_validator_updates: Vec<ValidatorUpdate>,
    /// Set in `end_epoch` and reset to `None` when `reward_notes` is called.
    pub reward_notes: Vec<(u64, Address)>,
    /// Records any updates to the token supply of some asset that happened in this block.
    pub supply_updates: BTreeMap<asset::Id, (asset::Denom, u64)>,
}

impl ValidatorSet {
    pub async fn new(reader: Reader, epoch: Epoch) -> Result<Self> {
        // Grab all validator info from the database. This will only happen when the
        // ValidatorSet is first instantiated.
        let block_validators = reader.validator_info(true).await?;

        // Initialize all state machine validator states to their current state from the block validators.
        let mut validator_set = BTreeMap::new();
        for validator in block_validators.iter() {
            validator_set.insert(validator.validator.identity_key.clone(), validator.clone());
        }

        Ok(ValidatorSet {
            validator_set,
            epoch: Some(epoch),
            next_base_rate: None,
            next_rates: None,
            validator_definitions: BTreeMap::new(),
            new_validators: Vec::new(),
            updated_validators: Vec::new(),
            slashed_validators: Vec::new(),
            reader,
            tm_validator_updates: Vec::new(),
            delegation_changes: BTreeMap::new(),
            reward_notes: Vec::new(),
            supply_updates: BTreeMap::new(),
        })
    }

    /// Called during `commit_block` and will reset internal state
    /// for the next block, as well as set `self.epoch` to the
    /// `new_epoch` value passed in.
    pub async fn commit_block(&mut self, new_epoch: Epoch) {
        if new_epoch.index != self.epoch.as_ref().unwrap().index {
            // This resets the rate and supply information
            // that only changes during epoch transitions.
            self.epoch = Some(new_epoch);
            self.next_base_rate = None;
            self.next_rates = None;
            self.reward_notes = Vec::new();
            self.supply_updates = BTreeMap::new();
        }

        // TODO: split per-block and per-epoch state
        // into separate structs wrapping the data

        // New, slashed, and updated validators can happen in any block,
        // not just on epoch transitions.
        self.new_validators = Vec::new();
        self.slashed_validators = Vec::new();
        self.validator_definitions = BTreeMap::new();
        self.updated_validators = Vec::new();
        self.tm_validator_updates = Vec::new();
        self.delegation_changes = BTreeMap::new();
    }

    // Called during `end_block`. Responsible for resolving conflicting ValidatorDefinitions
    // that came in during the block and updating `validator_set` with new validator info.
    //
    // Will update the epoch to whatever is passed in.
    //
    // Any *state changes* (i.e. ValidatorState) should have already been applied to `validator_set`
    // by the time this is called!
    pub async fn end_block(&mut self, epoch: Epoch) -> Result<()> {
        self.epoch = Some(epoch.clone());
        // This closure is used to generate a new ValidatorInfo from a ValidatorDefinition.
        // This should *only* be called for *new validators* as it sets the validator's state
        // to Inactive and sets default rate data!
        let make_validator = |v: VerifiedValidatorDefinition| -> ValidatorInfo {
            ValidatorInfo {
                validator: v.validator.clone(),
                status: ValidatorStatus {
                    identity_key: v.validator.identity_key.clone(),
                    // Voting power for inactive validators is 0
                    voting_power: 0,
                    state: ValidatorState::Inactive,
                },
                rate_data: RateData {
                    identity_key: v.validator.identity_key.clone(),
                    epoch_index: epoch.index,
                    // Validator reward rate is held constant for inactive validators.
                    // Stake committed to inactive validators earns no rewards.
                    validator_reward_rate: 0,
                    // Exchange rate for inactive validators is held constant
                    // and starts at 1
                    validator_exchange_rate: 1,
                },
            }
        };

        // This will hold a single deterministically chosen validator definition for every identity key
        // we received validator definitions for.
        let mut resolved_validator_definitions: BTreeMap<IdentityKey, VerifiedValidatorDefinition> =
            BTreeMap::new();
        // Any conflicts in validator definitions added to the pending block need to be resolved.
        // TODO: this code should be tested to ensure changes don't break the ordering.
        for (ik, defs) in self.validator_definitions.iter_mut() {
            // Ensure the definitions are sorted by descending sequence number
            defs.sort_by(|a, b| {
                b.validator
                    .sequence_number
                    .cmp(&a.validator.sequence_number)
            });

            if defs.len() == 1 {
                // If there was only one definition for an identity key, use it.
                resolved_validator_definitions.insert(ik.clone(), defs[0].clone());
                continue;
            }

            // Sort the validator definitions into buckets by their sequence number.
            let mut new_validator_definitions_by_seq: Vec<(u32, Vec<VerifiedValidatorDefinition>)> =
                Vec::new();
            for def in defs.iter() {
                let seq = def.validator.sequence_number;
                let def = def.clone();

                // If we haven't seen this sequence number before, create a new bucket.
                if !new_validator_definitions_by_seq
                    .iter()
                    .any(|(s, _)| *s == seq)
                {
                    new_validator_definitions_by_seq.push((seq, vec![def]));
                } else {
                    // Otherwise, add the definition to the existing bucket.
                    let mut found = false;
                    for (s, defs) in new_validator_definitions_by_seq.iter_mut() {
                        if *s == seq {
                            defs.push(def);
                            found = true;
                            break;
                        }
                    }
                    assert!(found);
                }
            }

            // The highest sequence number bucket wins.
            let highest_seq_bucket = &mut new_validator_definitions_by_seq[0];

            // Sort any conflicting definitions for the highest sequence number by
            // their signatures to get a deterministic ordering.
            highest_seq_bucket.1.sort_by(|a, b| {
                let a_sig = a.auth_sig.to_bytes();
                let b_sig = b.auth_sig.to_bytes();
                a_sig.cmp(&b_sig)
            });

            // Our pick will be the first definition in the bucket after sorting by signature.
            resolved_validator_definitions.insert(ik.clone(), highest_seq_bucket.1[0].clone());
        }

        // Now that we have resolved all validator definitions, we can determine the validator
        // changes that occurred in this block.
        for (ik, def) in resolved_validator_definitions.iter() {
            if self.validators().any(|v| v.borrow().identity_key == *ik) {
                // If this is an existing validator, there will need to be a database UPDATE query.
                // The existing state will be maintained but the validator configuration will change
                // to the new definition.
                //
                // Funding stream, rate, and voting power calculations will occur for this validator
                // during end_epoch. The old values are maintained until then.
                let mut validator_info = self.validator_set.get_mut(ik).unwrap();

                // Update the internal validator configuration
                // The validator definition was already verified during verify_stateless/verify_stateful
                // Replace the validator within the validator set with the new definition
                // but keep the current status/state/rate data
                validator_info.validator = def.validator.clone();

                // Add the validator to the block's updated validators list so an UPDATE query will be generated in
                // `commit_block`.
                self.updated_validators.push(validator_info.clone());
            } else {
                // Create the new validator's ValidatorInfo struct.
                // The status will default to Inactive for new validators.
                let new_validator = make_validator(def.clone());

                // Add the validator to the internal validator set.
                self.add_validator(new_validator.clone());

                // Add the validator to the block's new validators list so an INSERT query will be generated in `commit_block`.
                self.new_validators.push(new_validator);
            }
        }

        // Set `self.tm_validator_updates` to the complete set of
        // validators and voting power. This must be the last step performed,
        // after all voting power calculations and validator state transitions have
        // been completed.
        //
        // TODO: It could be more efficient to only return the power of
        // updated validators.
        self.tm_validator_updates = self
            .validators_info()
            .map(|v| {
                let v = v.borrow();
                // if the validator is non-Active, set their voting power as
                // returned to Tendermint to 0. Only Active validators report
                // voting power to Tendermint.
                let power = if v.status.state == ValidatorState::Active {
                    v.status.voting_power as u64
                } else {
                    0
                };
                let validator = &v.validator;
                let pub_key = validator.consensus_key;
                Ok(tendermint::abci::types::ValidatorUpdate {
                    pub_key,
                    power: power.try_into()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(())
    }

    pub fn update_delegations(&mut self, delegation_changes: &BTreeMap<IdentityKey, i64>) {
        // Tally the delegation changes in this transaction
        for (identity_key, delegation_change) in delegation_changes {
            *self
                .delegation_changes
                .entry(identity_key.clone())
                .or_insert(0) += delegation_change;
        }
    }

    /// Called during the commit phase of a block. Will return the current status of
    /// all validators within the state machine.
    pub fn next_validator_statuses(&self) -> Vec<ValidatorStatus> {
        self.validators_info()
            .map(|v| v.borrow().status.clone())
            .collect()
    }

    /// Called during `end_epoch`. Will perform state transitions to validators based
    /// on changes to voting power that occurred in this epoch.
    pub fn process_epoch_transitions(
        &mut self,
        active_validator_limit: u64,
        current_epoch: Epoch,
        unbonding_epochs: u64,
    ) -> Result<()> {
        // Sort the next validator states by voting power.
        // Dislike this clone, but the borrow checker was complaining about the loop modifying itself
        // when I tried using the validators_info() iterator.
        let mut validators_info = self
            .validator_set
            .iter()
            .map(|(_, v)| (v.clone()))
            .collect::<Vec<_>>();
        validators_info.sort_by(|a, b| {
            a.borrow()
                .status
                .voting_power
                .cmp(&b.borrow().status.voting_power)
        });
        let top_validators = validators_info
            .iter()
            .take(active_validator_limit as usize)
            .map(|v| v.borrow().validator.identity_key.clone())
            .collect::<Vec<_>>();
        for vi in &validators_info {
            let validator_status = &vi.borrow().status.clone();
            if validator_status.state == ValidatorState::Inactive
                || matches!(
                    validator_status.state,
                    ValidatorState::Unbonding { unbonding_epoch: _ }
                )
            {
                // If an Inactive or Unbonding validator is in the top `active_validator_limit` based
                // on voting power and the delegation pool has a nonzero balance,
                // then the validator should be moved to the Active state.
                if top_validators.contains(&validator_status.identity_key) {
                    // TODO: How do we check the delegation pool balance here?
                    // https://github.com/penumbra-zone/penumbra/issues/445
                    self.activate_validator(vi.borrow().validator.consensus_key.clone())?;
                }
            } else if validator_status.state == ValidatorState::Active {
                // An Active validator could also be displaced and move to the
                // Unbonding state.
                if !top_validators.contains(&validator_status.identity_key) {
                    self.unbond_validator(
                        vi.borrow().validator.consensus_key.clone(),
                        current_epoch.index + unbonding_epochs,
                    )?;
                }
            }

            // An Unbonding validator can become Inactive if the unbonding period expires
            // and the validator is still in Unbonding state
            if let ValidatorState::Unbonding { unbonding_epoch } = validator_status.state {
                if unbonding_epoch <= current_epoch.index {
                    self.deactivate_validator(vi.borrow().validator.consensus_key.clone())?;
                }
            };
        }

        Ok(())
    }

    /// Called during `end_epoch`. Will calculate validator changes that can only happen during epoch changes
    /// such as rate updates.
    // pub async fn end_epoch(&mut self) -> Result<()> {
    pub fn end_epoch(&'_ mut self) -> impl Future<Output = Result<()>> + Send + Unpin + '_ {
        let chain_params = self.reader.chain_params_rx().borrow();
        let unbonding_epochs: u64 = chain_params.unbonding_epochs;
        let active_validator_limit: u64 = chain_params.active_validator_limit;
        drop(chain_params);

        Box::pin(async move {
            // We've finished processing the last block of `epoch`, so we've
            // crossed the epoch boundary, and (prev | current | next) are:
            let prev_epoch = &self
                .epoch
                .clone()
                .expect("epoch must already have been set");
            let current_epoch = prev_epoch.next();
            let _next_epoch = current_epoch.next();
            let current_base_rate = self.reader.base_rate_data(current_epoch.index).await?;

            /// FIXME: set this less arbitrarily, and allow this to be set per-epoch
            /// 3bps -> 11% return over 365 epochs, why not
            const BASE_REWARD_RATE: u64 = 3_0000;

            let next_base_rate = current_base_rate.next(BASE_REWARD_RATE);

            // rename to curr_rate so it lines up with next_rate (same # chars)
            tracing::debug!(curr_base_rate = ?current_base_rate);
            tracing::debug!(?next_base_rate);

            let mut staking_token_supply = self
                .reader
                .asset_lookup(*STAKING_TOKEN_ASSET_ID)
                .await?
                .map(|info| info.total_supply)
                .unwrap();

            let mut next_rates = Vec::new();
            let mut reward_notes = Vec::new();
            let mut supply_updates = Vec::new();

            // this is a bit complicated: because we're in the EndBlock phase, and the
            // delegations in this block have not yet been committed, we have to combine
            // the delegations in pending_block with the ones already committed to the
            // state. otherwise the delegations committed in the epoch threshold block
            // would be lost.
            let mut delegation_changes = self.reader.delegation_changes(prev_epoch.index).await?;
            for (id_key, delta) in &self.delegation_changes {
                *delegation_changes.entry(id_key.clone()).or_insert(0) += delta;
            }

            // steps (foreach validator):
            // - get the total token supply for the validator's delegation tokens
            // - process the updates to the token supply:
            //   - collect all delegations occurring in previous epoch and apply them (adds to supply);
            //   - collect all undelegations started in previous epoch and apply them (reduces supply);
            // - feed the updated (current) token supply into current_rates.voting_power()
            // - persist both the current voting power and the current supply
            //
            for v in &mut self.validator_set {
                let validator = v.1;
                let current_rate = validator.rate_data.clone();

                let funding_streams = self
                    .reader
                    .funding_streams(validator.validator.identity_key.clone())
                    .await?;

                let next_rate = current_rate.next(
                    &next_base_rate,
                    funding_streams.as_ref(),
                    &validator.status.state,
                );
                let identity_key = validator.validator.identity_key.clone();

                let delegation_delta = delegation_changes.get(&identity_key).unwrap_or(&0i64);

                let delegation_amount = delegation_delta.abs() as u64;
                let unbonded_amount = current_rate.unbonded_amount(delegation_amount);

                let mut delegation_token_supply = self
                    .reader
                    .asset_lookup(identity_key.delegation_token().id())
                    .await?
                    .map(|info| info.total_supply)
                    .unwrap_or(0);

                if *delegation_delta > 0 {
                    // net delegation: subtract the unbonded amount from the staking token supply
                    staking_token_supply =
                        staking_token_supply.checked_sub(unbonded_amount).unwrap();
                    delegation_token_supply = delegation_token_supply
                        .checked_add(delegation_amount)
                        .unwrap();
                } else {
                    // net undelegation: add the unbonded amount to the staking token supply
                    staking_token_supply =
                        staking_token_supply.checked_add(unbonded_amount).unwrap();
                    delegation_token_supply = delegation_token_supply
                        .checked_sub(delegation_amount)
                        .unwrap();
                }

                // update the delegation token supply
                supply_updates.push((
                    identity_key.delegation_token().id(),
                    identity_key.delegation_token().denom(),
                    delegation_token_supply,
                ));
                let voting_power = next_rate.voting_power(delegation_token_supply, &next_base_rate);

                // Update the status of the validator within the validator set
                // with the newly calculated voting power.
                validator.status.voting_power = voting_power;

                // Only Active validators produce commission rewards
                if validator.status.state == ValidatorState::Active {
                    // distribute validator commission
                    for stream in funding_streams {
                        let commission_reward_amount = stream.reward_amount(
                            delegation_token_supply,
                            &next_base_rate,
                            &current_base_rate,
                        );

                        reward_notes.push((commission_reward_amount, stream.address));
                    }
                }

                // rename to curr_rate so it lines up with next_rate (same # chars)
                tracing::debug!(curr_rate = ?current_rate);
                tracing::debug!(?next_rate);
                tracing::debug!(?delegation_delta);
                tracing::debug!(?delegation_token_supply);

                next_rates.push(next_rate);
            }

            // State transitions on epoch change are handled here
            // after all rates have been calculated
            self.process_epoch_transitions(
                active_validator_limit,
                current_epoch,
                unbonding_epochs,
            )?;

            for supply_update in supply_updates {
                self.add_supply_update(supply_update.0, supply_update.1, supply_update.2);
            }

            tracing::debug!(?staking_token_supply);

            self.next_rates = Some(next_rates);
            self.next_base_rate = Some(next_base_rate);
            self.reward_notes = reward_notes;
            self.supply_updates.insert(
                *STAKING_TOKEN_ASSET_ID,
                (STAKING_TOKEN_DENOM.clone(), staking_token_supply),
            );

            Ok(())
        })
    }

    pub fn add_supply_update(&mut self, id: Id, denom: Denom, token_supply: u64) {
        self.supply_updates.insert(id, (denom, token_supply));
    }

    // This should *only* be called during `end_block` as validators don't
    // get exposed to consensus until then.
    pub fn add_validator(&mut self, validator: ValidatorInfo) {
        self.validator_set
            .insert(validator.validator.identity_key.clone(), validator);
    }

    pub fn update_supply_for_denom(&mut self, denom: Denom, amount: u64) {
        self.supply_updates
            .entry(denom.id())
            .or_insert((denom, 0))
            .1 += amount;
    }

    /// This keeps track of validator definitions received during the block.
    /// Any conflicts will be resolved and resulting changes to the validator set
    /// will be applied during end_block.
    ///
    /// Validator definitions must have been already validated by verify_stateful/verify_stateless
    /// prior to calling this method.
    pub fn add_validator_definition(&mut self, validator_definition: VerifiedValidatorDefinition) {
        let identity_key = validator_definition.validator.identity_key.clone();

        self.validator_definitions
            .entry(identity_key)
            .or_insert_with(Vec::new)
            .push(validator_definition);
    }

    pub fn get_validator_info(&self, identity_key: &IdentityKey) -> Option<&ValidatorInfo> {
        self.validator_set.get(identity_key)
    }

    pub fn get_state(&self, identity_key: &IdentityKey) -> Option<&ValidatorState> {
        self.validator_set
            .get(identity_key)
            .map(|x| &x.status.state)
    }

    pub fn validators(&self) -> impl Iterator<Item = impl Borrow<&'_ Validator>> {
        self.validator_set.iter().map(|v| &v.1.validator)
    }

    pub fn validators_info(
        &self,
    ) -> impl Clone + Iterator<Item = impl Borrow<&'_ ValidatorInfo> + BorrowMut<&'_ ValidatorInfo>>
    {
        self.validator_set.iter().map(|v| v.1)
    }

    /// Returns all validators that are currently in the `Slashed` state.
    pub fn slashed_validators(&self) -> impl Iterator<Item = impl Borrow<&'_ Validator>> {
        self.validator_set
            .iter()
            .filter(|v| v.1.status.state == ValidatorState::Slashed)
            .map(|v| &v.1.validator)
    }

    pub fn unslashed_validators(&self) -> impl Iterator<Item = impl Borrow<&'_ Validator>> {
        // validators: Option<impl IntoIterator<Item = impl Borrow<&'a IdentityKey>>>,
        self.validator_set
            .iter()
            // Return all validators that are *not slashed*
            .filter(|v| v.1.status.state != ValidatorState::Slashed)
            .map(|v| &v.1.validator)
    }

    /// Mark a validator as deactivated. Only validators in the unbonding state
    /// can be deactivated.
    pub fn deactivate_validator(&mut self, ck: PublicKey) -> Result<()> {
        // Don't love this clone.
        let validator = self.get_validator_by_consensus_key(&ck)?.clone();

        let current_info = self
            .get_validator_info(&validator.identity_key)
            .ok_or(anyhow::anyhow!("Validator not found in state machine"))?;
        let current_state = current_info.status.state;

        match current_state {
            ValidatorState::Unbonding { unbonding_epoch: _ } => {
                self.validator_set
                    .get_mut(&validator.identity_key)
                    .ok_or_else(|| anyhow::anyhow!("Validator not found"))?
                    .status
                    .state = ValidatorState::Inactive;
                Ok(())
            }
            _ => Err(anyhow::anyhow!(
                "Validator {} is not in unbonding state",
                validator.identity_key
            )),
        }
    }

    // Activate a validator. Only validators in the inactive or unbonding state
    // may be activated.
    pub fn activate_validator(&mut self, ck: PublicKey) -> Result<()> {
        // Don't love this clone.
        let validator = self.get_validator_by_consensus_key(&ck)?.clone();

        let current_info = self
            .get_validator_info(&validator.identity_key)
            .ok_or(anyhow::anyhow!("Validator not found in state machine"))?;
        let current_state = current_info.status.state;

        let mut mark_active = |validator: &Validator| -> Result<()> {
            self.validator_set
                .get_mut(&validator.identity_key)
                .ok_or_else(|| anyhow::anyhow!("Validator not found"))?
                .status
                .state = ValidatorState::Active;
            Ok(())
        };

        match current_state {
            ValidatorState::Inactive => mark_active(&validator),
            // The unbonding epoch is not checked here. It is checked in the
            // consensus worker.
            ValidatorState::Unbonding { unbonding_epoch: _ } => mark_active(&validator),
            _ => Err(anyhow::anyhow!(
                "only validators in the inactive or unbonding state may be activated"
            )),
        }
    }

    // Marks a validator as slashed. Only validators in the active or unbonding
    // state may be slashed.
    pub fn slash_validator(&mut self, ck: &PublicKey, slashing_penalty: u64) -> Result<()> {
        // Don't love this clone.
        let validator = self.get_validator_by_consensus_key(ck)?.clone();

        self.slashed_validators.push(validator.identity_key.clone());

        let current_info = self
            .get_validator_info(&validator.identity_key)
            .ok_or(anyhow::anyhow!("Validator not found in state machine"))?;
        let current_state = current_info.status.state;

        let mut mark_slashed = |validator: &Validator| -> Result<()> {
            self.validator_set
                .get_mut(&validator.identity_key)
                .ok_or_else(|| anyhow::anyhow!("Validator not found"))?
                .status
                .state = ValidatorState::Slashed;
            self.validator_set
                .get_mut(&validator.identity_key)
                .ok_or_else(|| anyhow::anyhow!("Validator not found"))?
                .rate_data
                .slash(slashing_penalty);
            Ok(())
        };

        match current_state {
            ValidatorState::Active => mark_slashed(&validator),
            ValidatorState::Unbonding { unbonding_epoch: _ } => mark_slashed(&validator),
            _ => Err(anyhow::anyhow!(
                "only validators in the active or unbonding state may be slashed"
            )),
        }
    }

    // Marks a validator as unbonding. Only validators in the active state
    // may begin unbonding.
    pub fn unbond_validator(&mut self, ck: PublicKey, unbonding_epoch: u64) -> Result<()> {
        // Don't love this clone.
        let validator = self.get_validator_by_consensus_key(&ck)?.clone();
        let current_info = self
            .get_validator_info(&validator.identity_key)
            .ok_or(anyhow::anyhow!("Validator not found in state machine"))?;
        let current_state = current_info.status.state;

        match current_state {
            ValidatorState::Active => {
                // Unbonding the validator means that it can no longer participate
                // in consensus, so its voting power is set to 0.
                self.validator_set
                    .get_mut(&validator.identity_key)
                    .ok_or_else(|| anyhow::anyhow!("Validator not found"))?
                    .status
                    .voting_power = 0;
                self.validator_set
                    .get_mut(&validator.identity_key)
                    .ok_or_else(|| anyhow::anyhow!("Validator not found"))?
                    .status
                    .state = ValidatorState::Unbonding { unbonding_epoch };
                Ok(())
            }
            _ => Err(anyhow::anyhow!(
                "only validators in the active state may begin unbonding"
            )),
        }
    }

    // Tendermint validators are referenced to us by their Tendermint consensus key,
    // but we reference them by their Penumbra identity key.
    pub fn get_validator_by_consensus_key(&self, ck: &PublicKey) -> Result<&Validator> {
        let validator = self
            .validator_set
            .iter()
            .find(|v| v.1.validator.consensus_key == *ck)
            .ok_or(anyhow::anyhow!("No validator found"))?;
        Ok(&validator.1.validator)
    }
}
