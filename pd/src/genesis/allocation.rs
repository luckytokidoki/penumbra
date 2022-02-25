use ark_ff::Zero;
use decaf377::Fq;

use penumbra_crypto::{asset, Address, Note, Value};
use penumbra_proto::{genesis as pb, Protobuf};

use serde::{Deserialize, Serialize};

/// A (transparent) genesis allocation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(
    try_from = "pb::genesis_app_state::Allocation",
    into = "pb::genesis_app_state::Allocation"
)]
pub struct Allocation {
    pub amount: u64,
    pub denom: String,
    pub address: Address,
}

impl From<Allocation> for pb::genesis_app_state::Allocation {
    fn from(a: Allocation) -> Self {
        pb::genesis_app_state::Allocation {
            amount: a.amount,
            denom: a.denom,
            address: Some(a.address.into()),
        }
    }
}

impl TryFrom<pb::genesis_app_state::Allocation> for Allocation {
    type Error = anyhow::Error;

    fn try_from(msg: pb::genesis_app_state::Allocation) -> Result<Self, Self::Error> {
        Ok(Allocation {
            amount: msg.amount,
            denom: msg.denom,
            address: msg
                .address
                .ok_or_else(|| anyhow::anyhow!("missing address field in proto"))?
                .try_into()?,
        })
    }
}

// Implement Debug manually so we can use the Display impl for the address,
// rather than dumping all the internal address components.
impl std::fmt::Debug for Allocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Allocation")
            .field("amount", &self.amount)
            .field("denom", &self.denom)
            .field("address", &self.address.to_string())
            .finish()
    }
}

impl Allocation {
    /// Obtain a note corresponding to this allocation.
    ///
    /// Note: to ensure determinism, this uses a zero blinding factor when
    /// creating the note. This is fine, because the genesis allocations are
    /// already public.
    pub fn note(&self) -> Result<Note, anyhow::Error> {
        Note::from_parts(
            *self.address.diversifier(),
            *self.address.transmission_key(),
            Value {
                amount: self.amount,
                asset_id: asset::REGISTRY
                    .parse_denom(&self.denom)
                    .ok_or_else(|| anyhow::anyhow!("invalid denomination"))?
                    .id(),
            },
            Fq::zero(),
        )
        .map_err(Into::into)
    }
}

impl Protobuf<pb::genesis_app_state::Allocation> for Allocation {}
