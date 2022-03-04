//! Transparent proofs for `MVP1` of the Penumbra system.

use std::convert::{TryFrom, TryInto};

use decaf377::FieldExt;
use decaf377_rdsa::{SpendAuth, VerificationKey};
use penumbra_proto::{transparent_proofs, Message, Protobuf};
use thiserror;

use crate::{asset, ka, keys, merkle, merkle::Hashable, note, value, Fq, Fr, Nullifier, Value};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Invalid spend auth randomizer")]
    InvalidSpendAuthRandomizer,
    #[error("Note commitment mismatch")]
    NoteCommitmentMismatch,
    #[error("Transmission key mismatch")]
    TransmissionKeyMismatch,
    #[error("Value commitment mismatch")]
    ValueCommitmentMismatch,
    #[error("Ephemeral public key mismatch")]
    EphemeralPublicKeyMismatch,
    #[error("Must not be an identity")]
    IdentityUnexpected,
    #[error("Unexpected depth for merkle path")]
    MerklePathMismatch,
    #[error("Merkle root mismatch")]
    MerkleRootMismatch,
    #[error("Invalid diversified address")]
    InvalidDiversifiedAddress,
    #[error("Bad nullifier")]
    BadNullifier,
    #[error("Transparent proof proto malformed")]
    ProtoMalformed,
}

/// Transparent proof for spending existing notes.
///
/// This structure keeps track of the auxiliary (private) inputs.
#[derive(Clone, Debug)]
pub struct SpendProof {
    // Path to the note being spent in the note commitment merkle tree.
    pub merkle_path: merkle::Path,
    // Position of the note being spent in the note commitment merkle tree.
    pub position: merkle::Position,
    // The diversified base for the address.
    pub g_d: decaf377::Element,
    // The transmission key for the address.
    pub pk_d: ka::Public,
    // The value of the note.
    pub value: Value,
    // The blinding factor used for generating the value commitment.
    pub v_blinding: Fr,
    // The note commitment.
    pub note_commitment: note::Commitment,
    // The blinding factor used for generating the note commitment.
    pub note_blinding: Fq,
    // The randomizer used for generating the randomized spend auth key.
    pub spend_auth_randomizer: Fr,
    // The spend authorization key.
    pub ak: VerificationKey<SpendAuth>,
    // The nullifier deriving key.
    pub nk: keys::NullifierKey,
}

impl SpendProof {
    /// Called to verify the proof using the provided public inputs.
    ///
    /// The public inputs are:
    /// * the merkle root of the note commitment tree,
    /// * value commitment of the note to be spent,
    /// * nullifier of the note to be spent,
    /// * the randomized verification spend key,
    pub fn verify(
        &self,
        anchor: merkle::Root,
        value_commitment: value::Commitment,
        nullifier: Nullifier,
        rk: VerificationKey<SpendAuth>,
    ) -> anyhow::Result<(), Error> {
        // Note commitment integrity.
        let s_component_transmission_key = Fq::from_bytes(self.pk_d.0);
        if let Ok(transmission_key_s) = s_component_transmission_key {
            let note_commitment_test =
                note::Commitment::new(self.note_blinding, self.value, self.g_d, transmission_key_s);

            if self.note_commitment != note_commitment_test {
                return Err(Error::NoteCommitmentMismatch);
            }
        } else {
            return Err(Error::TransmissionKeyMismatch);
        }

        // Merkle path integrity.
        // 1. Check the Merkle path is a depth of `merkle::DEPTH`.
        if self.merkle_path.1.len() != merkle::DEPTH {
            return Err(Error::MerklePathMismatch);
        }

        // 2. Check the Merkle path leads to the expected anchor (`merkle::Root`).
        let mut cur = self.note_commitment;

        // This logic is from `incrementalmerkletree`'s `compute_root_from_auth_path` function which is
        // `pub(crate)` so is included below.
        let mut lvl = merkle::Altitude::zero();
        for (i, v) in self.merkle_path.1.iter().enumerate().map(|(i, v)| {
            (
                ((<usize>::try_from(self.position).unwrap() >> i) & 1) == 1,
                v,
            )
        }) {
            if i {
                cur = note::Commitment::combine(lvl, v, &cur);
            } else {
                cur = note::Commitment::combine(lvl, &cur, v);
            }
            lvl = lvl + 1;
        }
        let expected_root = merkle::Root(cur.0);
        if expected_root != anchor {
            return Err(Error::MerkleRootMismatch);
        }

        // Value commitment integrity.
        if self.value.commit(self.v_blinding) != value_commitment {
            return Err(Error::ValueCommitmentMismatch);
        }

        // The use of decaf means that we do not need to check that the
        // diversified basepoint is of small order. However we instead
        // check it is not identity.
        if self.g_d.is_identity() || self.ak.is_identity() {
            return Err(Error::IdentityUnexpected);
        }

        // Nullifier integrity.
        if nullifier
            != self
                .nk
                .derive_nullifier(self.position, &self.note_commitment)
        {
            return Err(Error::BadNullifier);
        }

        // Spend authority.
        let rk_bytes: [u8; 32] = rk.into();
        let rk_test = self.ak.randomize(&self.spend_auth_randomizer);
        let rk_test_bytes: [u8; 32] = rk_test.into();
        if rk_bytes != rk_test_bytes {
            return Err(Error::InvalidSpendAuthRandomizer);
        }

        // Diversified address integrity.
        let fvk = keys::FullViewingKey::from_components(self.ak, self.nk);
        let ivk = fvk.incoming();
        if self.pk_d != ivk.diversified_public(&self.g_d) {
            return Err(Error::InvalidDiversifiedAddress);
        }

        Ok(())
    }
}

/// Transparent proof for new note creation.
///
/// This structure keeps track of the auxiliary (private) inputs.
#[derive(Clone, Debug)]
pub struct OutputProof {
    // The diversified base for the destination address.
    pub g_d: decaf377::Element,
    // The transmission key for the destination address.
    pub pk_d: ka::Public,
    // The value of the newly created note.
    pub value: Value,
    // The blinding factor used for generating the value commitment.
    pub v_blinding: Fr,
    // The blinding factor used for generating the note commitment.
    pub note_blinding: Fq,
    // The ephemeral secret key that corresponds to the public key.
    pub esk: ka::Secret,
}

impl OutputProof {
    /// Called to verify the proof using the provided public inputs.
    ///
    /// The public inputs are:
    /// * value commitment of the new note,
    /// * note commitment of the new note,
    /// * the ephemeral public key used to generate the new note.
    pub fn verify(
        &self,
        value_commitment: value::Commitment,
        note_commitment: note::Commitment,
        epk: ka::Public,
    ) -> anyhow::Result<(), Error> {
        // Note commitment integrity.
        let s_component_transmission_key = Fq::from_bytes(self.pk_d.0);
        if let Ok(transmission_key_s) = s_component_transmission_key {
            let note_commitment_test =
                note::Commitment::new(self.note_blinding, self.value, self.g_d, transmission_key_s);

            if note_commitment != note_commitment_test {
                return Err(Error::NoteCommitmentMismatch);
            }
        } else {
            return Err(Error::TransmissionKeyMismatch);
        }

        // Value commitment integrity.
        if value_commitment != -self.value.commit(self.v_blinding) {
            return Err(Error::ValueCommitmentMismatch);
        }

        // Ephemeral public key integrity.
        if self.esk.diversified_public(&self.g_d) != epk {
            return Err(Error::EphemeralPublicKeyMismatch);
        }

        // The use of decaf means that we do not need to check that the
        // diversified basepoint is of small order. However we instead
        // check it is not identity.
        if self.g_d.is_identity() {
            return Err(Error::IdentityUnexpected);
        }

        Ok(())
    }
}

// Conversions

impl Protobuf<transparent_proofs::SpendProof> for SpendProof {}

impl From<SpendProof> for transparent_proofs::SpendProof {
    fn from(msg: SpendProof) -> Self {
        let ak_bytes: [u8; 32] = msg.ak.into();
        let nk_bytes: [u8; 32] = msg.nk.0.to_bytes();
        transparent_proofs::SpendProof {
            merkle_path_field_0: u64::from(msg.merkle_path.0) as u32,
            merkle_path_field_1: msg
                .merkle_path
                .1
                .into_iter()
                .map(|x| x.0.to_bytes().into())
                .collect(),
            position: msg.position.into(),
            g_d: msg.g_d.compress().0.to_vec(),
            pk_d: msg.pk_d.0.to_vec(),
            value_amount: msg.value.amount,
            value_asset_id: msg.value.asset_id.0.to_bytes().to_vec(),
            v_blinding: msg.v_blinding.to_bytes().to_vec(),
            note_commitment: msg.note_commitment.0.to_bytes().to_vec(),
            note_blinding: msg.note_blinding.to_bytes().to_vec(),
            spend_auth_randomizer: msg.spend_auth_randomizer.to_bytes().to_vec(),
            ak: ak_bytes.into(),
            nk: nk_bytes.into(),
        }
    }
}

impl TryFrom<transparent_proofs::SpendProof> for SpendProof {
    type Error = Error;

    fn try_from(proto: transparent_proofs::SpendProof) -> anyhow::Result<Self, Self::Error> {
        let g_d_bytes: [u8; 32] = proto.g_d.try_into().map_err(|_| Error::ProtoMalformed)?;
        let g_d_encoding = decaf377::Encoding(g_d_bytes);

        let v_blinding_bytes: [u8; 32] = proto.v_blinding[..]
            .try_into()
            .map_err(|_| Error::ProtoMalformed)?;

        let ak_bytes: [u8; 32] = (proto.ak[..])
            .try_into()
            .map_err(|_| Error::ProtoMalformed)?;
        let ak = ak_bytes.try_into().map_err(|_| Error::ProtoMalformed)?;

        let mut merkle_path_vec = Vec::<note::Commitment>::new();
        for merkle_path_segment in proto.merkle_path_field_1 {
            merkle_path_vec.push(
                merkle_path_segment[..]
                    .try_into()
                    .map_err(|_| Error::ProtoMalformed)?,
            );
        }

        Ok(SpendProof {
            merkle_path: ((proto.merkle_path_field_0 as usize).into(), merkle_path_vec),
            position: (proto.position as usize).into(),
            g_d: g_d_encoding
                .decompress()
                .map_err(|_| Error::ProtoMalformed)?,
            pk_d: ka::Public(proto.pk_d.try_into().map_err(|_| Error::ProtoMalformed)?),
            value: Value {
                amount: proto.value_amount,
                asset_id: asset::Id(
                    Fq::from_bytes(
                        proto
                            .value_asset_id
                            .try_into()
                            .map_err(|_| Error::ProtoMalformed)?,
                    )
                    .map_err(|_| Error::ProtoMalformed)?,
                ),
            },
            v_blinding: Fr::from_bytes(v_blinding_bytes).map_err(|_| Error::ProtoMalformed)?,
            note_commitment: (proto.note_commitment[..])
                .try_into()
                .map_err(|_| Error::ProtoMalformed)?,
            note_blinding: Fq::from_bytes(
                proto.note_blinding[..]
                    .try_into()
                    .map_err(|_| Error::ProtoMalformed)?,
            )
            .map_err(|_| Error::ProtoMalformed)?,
            spend_auth_randomizer: Fr::from_bytes(
                proto.spend_auth_randomizer[..]
                    .try_into()
                    .map_err(|_| Error::ProtoMalformed)?,
            )
            .map_err(|_| Error::ProtoMalformed)?,
            ak,
            nk: keys::NullifierKey(
                Fq::from_bytes(proto.nk[..].try_into().map_err(|_| Error::ProtoMalformed)?)
                    .map_err(|_| Error::ProtoMalformed)?,
            ),
        })
    }
}

impl Protobuf<transparent_proofs::OutputProof> for OutputProof {}

impl From<OutputProof> for transparent_proofs::OutputProof {
    fn from(msg: OutputProof) -> Self {
        transparent_proofs::OutputProof {
            g_d: msg.g_d.compress().0.to_vec(),
            pk_d: msg.pk_d.0.to_vec(),
            value_amount: msg.value.amount,
            value_asset_id: msg.value.asset_id.0.to_bytes().to_vec(),
            v_blinding: msg.v_blinding.to_bytes().to_vec(),
            note_blinding: msg.note_blinding.to_bytes().to_vec(),
            esk: msg.esk.to_bytes().to_vec(),
        }
    }
}

impl TryFrom<transparent_proofs::OutputProof> for OutputProof {
    type Error = Error;

    fn try_from(proto: transparent_proofs::OutputProof) -> anyhow::Result<Self, Self::Error> {
        let g_d_bytes: [u8; 32] = proto.g_d.try_into().map_err(|_| Error::ProtoMalformed)?;
        let g_d_encoding = decaf377::Encoding(g_d_bytes);

        let v_blinding_bytes: [u8; 32] = proto.v_blinding[..]
            .try_into()
            .map_err(|_| Error::ProtoMalformed)?;

        let esk_bytes: [u8; 32] = proto.esk[..]
            .try_into()
            .map_err(|_| Error::ProtoMalformed)?;
        let esk = ka::Secret::new_from_field(
            Fr::from_bytes(esk_bytes).map_err(|_| Error::ProtoMalformed)?,
        );

        Ok(OutputProof {
            g_d: g_d_encoding
                .decompress()
                .map_err(|_| Error::ProtoMalformed)?,
            pk_d: ka::Public(proto.pk_d.try_into().map_err(|_| Error::ProtoMalformed)?),
            value: Value {
                amount: proto.value_amount,
                asset_id: asset::Id(
                    Fq::from_bytes(
                        proto
                            .value_asset_id
                            .try_into()
                            .map_err(|_| Error::ProtoMalformed)?,
                    )
                    .map_err(|_| Error::ProtoMalformed)?,
                ),
            },
            v_blinding: Fr::from_bytes(v_blinding_bytes).map_err(|_| Error::ProtoMalformed)?,
            note_blinding: Fq::from_bytes(
                proto.note_blinding[..]
                    .try_into()
                    .map_err(|_| Error::ProtoMalformed)?,
            )
            .map_err(|_| Error::ProtoMalformed)?,
            esk,
        })
    }
}

impl From<SpendProof> for Vec<u8> {
    fn from(spend_proof: SpendProof) -> Vec<u8> {
        let protobuf_serialized_proof: transparent_proofs::SpendProof = spend_proof.into();
        protobuf_serialized_proof.encode_to_vec()
    }
}

impl TryFrom<&[u8]> for SpendProof {
    type Error = Error;

    fn try_from(bytes: &[u8]) -> Result<SpendProof, Self::Error> {
        let protobuf_serialized_proof =
            transparent_proofs::SpendProof::decode(bytes).map_err(|_| Error::ProtoMalformed)?;
        protobuf_serialized_proof
            .try_into()
            .map_err(|_| Error::ProtoMalformed)
    }
}

impl From<OutputProof> for Vec<u8> {
    fn from(output_proof: OutputProof) -> Vec<u8> {
        let protobuf_serialized_proof: transparent_proofs::OutputProof = output_proof.into();
        protobuf_serialized_proof.encode_to_vec()
    }
}

impl TryFrom<&[u8]> for OutputProof {
    type Error = Error;

    fn try_from(bytes: &[u8]) -> Result<OutputProof, Self::Error> {
        let protobuf_serialized_proof =
            transparent_proofs::OutputProof::decode(bytes).map_err(|_| Error::ProtoMalformed)?;
        protobuf_serialized_proof
            .try_into()
            .map_err(|_| Error::ProtoMalformed)
    }
}

#[cfg(test)]
mod tests {
    use ark_ff::UniformRand;
    use rand_core::OsRng;

    use super::*;
    use crate::{
        keys::{SeedPhrase, SpendKey, SpendSeed},
        merkle,
        merkle::{Frontier, Tree, TreeExt},
        note, Note, Value,
    };

    #[test]
    fn test_output_proof_verification_success() {
        let mut rng = OsRng;

        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_recipient = SpendKey::new(spend_seed);
        let fvk_recipient = sk_recipient.full_viewing_key();
        let ivk_recipient = fvk_recipient.incoming();
        let (dest, _dtk_d) = ivk_recipient.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);
        let note = Note::generate(&mut rng, &dest, value_to_send);
        let esk = ka::Secret::new(&mut rng);
        let epk = esk.diversified_public(&note.diversified_generator());

        let proof = OutputProof {
            g_d: *dest.diversified_generator(),
            pk_d: *dest.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_blinding: note.note_blinding(),
            esk,
        };

        assert!(proof
            .verify(-value_to_send.commit(v_blinding), note.commit(), epk)
            .is_ok());
    }

    #[test]
    fn test_output_proof_verification_note_commitment_integrity_failure() {
        let mut rng = OsRng;

        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_recipient = SpendKey::new(spend_seed);
        let fvk_recipient = sk_recipient.full_viewing_key();
        let ivk_recipient = fvk_recipient.incoming();
        let (dest, _dtk_d) = ivk_recipient.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);
        let note = Note::generate(&mut rng, &dest, value_to_send);
        let esk = ka::Secret::new(&mut rng);
        let epk = esk.diversified_public(&note.diversified_generator());

        let proof = OutputProof {
            g_d: *dest.diversified_generator(),
            pk_d: *dest.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_blinding: note.note_blinding(),
            esk,
        };

        let incorrect_note_commitment = note::Commitment::new(
            Fq::rand(&mut rng),
            value_to_send,
            note.diversified_generator(),
            note.transmission_key_s(),
        );

        assert!(proof
            .verify(
                -value_to_send.commit(v_blinding),
                incorrect_note_commitment,
                epk
            )
            .is_err());
    }

    #[test]
    fn test_output_proof_verification_value_commitment_integrity_failure() {
        let mut rng = OsRng;

        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_recipient = SpendKey::new(spend_seed);
        let fvk_recipient = sk_recipient.full_viewing_key();
        let ivk_recipient = fvk_recipient.incoming();
        let (dest, _dtk_d) = ivk_recipient.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);
        let note = Note::generate(&mut rng, &dest, value_to_send);
        let esk = ka::Secret::new(&mut rng);
        let correct_epk = esk.diversified_public(&note.diversified_generator());

        let proof = OutputProof {
            g_d: *dest.diversified_generator(),
            pk_d: *dest.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_blinding: note.note_blinding(),
            esk,
        };
        let incorrect_value_commitment = value_to_send.commit(Fr::rand(&mut rng));

        assert!(proof
            .verify(incorrect_value_commitment, note.commit(), correct_epk)
            .is_err());
    }

    #[test]
    fn test_output_proof_verification_ephemeral_public_key_integrity_failure() {
        let mut rng = OsRng;

        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_recipient = SpendKey::new(spend_seed);
        let fvk_recipient = sk_recipient.full_viewing_key();
        let ivk_recipient = fvk_recipient.incoming();
        let (dest, _dtk_d) = ivk_recipient.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);
        let note = Note::generate(&mut rng, &dest, value_to_send);
        let esk = ka::Secret::new(&mut rng);

        let proof = OutputProof {
            g_d: *dest.diversified_generator(),
            pk_d: *dest.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_blinding: note.note_blinding(),
            esk,
        };
        let incorrect_esk = ka::Secret::new(&mut rng);
        let incorrect_epk = incorrect_esk.diversified_public(&note.diversified_generator());

        assert!(proof
            .verify(
                -value_to_send.commit(v_blinding),
                note.commit(),
                incorrect_epk
            )
            .is_err());
    }

    #[test]
    fn test_output_proof_verification_identity_check_failure() {
        let mut rng = OsRng;

        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_recipient = SpendKey::new(spend_seed);
        let fvk_recipient = sk_recipient.full_viewing_key();
        let ivk_recipient = fvk_recipient.incoming();
        let (dest, _dtk_d) = ivk_recipient.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);
        let note = Note::generate(&mut rng, &dest, value_to_send);
        let esk = ka::Secret::new(&mut rng);
        let epk = esk.diversified_public(&note.diversified_generator());

        let proof = OutputProof {
            g_d: decaf377::Element::default(),
            pk_d: *dest.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_blinding: note.note_blinding(),
            esk,
        };

        assert!(proof
            .verify(-value_to_send.commit(v_blinding), note.commit(), epk)
            .is_err());
    }

    #[test]
    fn test_spend_proof_verification_success() {
        let mut rng = OsRng;

        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_sender = SpendKey::new(spend_seed);
        let fvk_sender = sk_sender.full_viewing_key();
        let ivk_sender = fvk_sender.incoming();
        let (sender, _dtk_d) = ivk_sender.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);

        let note = Note::generate(&mut rng, &sender, value_to_send);
        let note_commitment = note.commit();
        let spend_auth_randomizer = Fr::rand(&mut rng);
        let rsk = sk_sender.spend_auth_key().randomize(&spend_auth_randomizer);
        let nk = *sk_sender.nullifier_key();
        let ak = sk_sender.spend_auth_key().into();
        let mut nct = merkle::BridgeTree::<note::Commitment, 32>::new(5);
        nct.append(&note_commitment);
        let anchor = nct.root2();
        nct.witness();
        let merkle_path = nct.authentication_path(&note_commitment).unwrap();

        let proof = SpendProof {
            merkle_path,
            position: 0.into(),
            g_d: *sender.diversified_generator(),
            pk_d: *sender.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_commitment,
            note_blinding: note.note_blinding(),
            spend_auth_randomizer,
            ak,
            nk,
        };

        let rk: VerificationKey<SpendAuth> = rsk.into();
        let nf = nk.derive_nullifier(0.into(), &note_commitment);
        assert!(proof
            .verify(anchor, value_to_send.commit(v_blinding), nf, rk)
            .is_ok());
    }

    #[test]
    fn test_spend_proof_verification_merkle_path_integrity_failure() {
        let mut rng = OsRng;
        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_sender = SpendKey::new(spend_seed);
        let fvk_sender = sk_sender.full_viewing_key();
        let ivk_sender = fvk_sender.incoming();
        let (sender, _dtk_d) = ivk_sender.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);

        let note = Note::generate(&mut rng, &sender, value_to_send);
        let note_commitment = note.commit();
        let spend_auth_randomizer = Fr::rand(&mut rng);
        let rsk = sk_sender.spend_auth_key().randomize(&spend_auth_randomizer);
        let nk = *sk_sender.nullifier_key();
        let ak = sk_sender.spend_auth_key().into();
        let mut nct = merkle::BridgeTree::<note::Commitment, 32>::new(5);
        let incorrect_anchor = nct.root2();
        nct.append(&note_commitment);
        nct.witness();
        let merkle_path = nct.authentication_path(&note_commitment).unwrap();

        let proof = SpendProof {
            merkle_path,
            position: 0.into(),
            g_d: *sender.diversified_generator(),
            pk_d: *sender.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_commitment,
            note_blinding: note.note_blinding(),
            spend_auth_randomizer,
            ak,
            nk,
        };

        let rk: VerificationKey<SpendAuth> = rsk.into();
        let nf = nk.derive_nullifier(0.into(), &note_commitment);
        assert!(proof
            .verify(incorrect_anchor, value_to_send.commit(v_blinding), nf, rk)
            .is_err());
    }

    #[test]
    fn test_spend_proof_verification_value_commitment_integrity_failure() {
        let mut rng = OsRng;
        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_sender = SpendKey::new(spend_seed);
        let fvk_sender = sk_sender.full_viewing_key();
        let ivk_sender = fvk_sender.incoming();
        let (sender, _dtk_d) = ivk_sender.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);
        let note = Note::generate(&mut rng, &sender, value_to_send);
        let note_commitment = note.commit();
        let spend_auth_randomizer = Fr::rand(&mut rng);
        let rsk = sk_sender.spend_auth_key().randomize(&spend_auth_randomizer);
        let nk = *sk_sender.nullifier_key();
        let ak = sk_sender.spend_auth_key().into();
        let mut nct = merkle::BridgeTree::<note::Commitment, 32>::new(5);
        nct.append(&note_commitment);
        nct.witness();
        let anchor = nct.root2();
        let merkle_path = nct.authentication_path(&note_commitment).unwrap();

        let proof = SpendProof {
            merkle_path,
            position: 0.into(),
            g_d: *sender.diversified_generator(),
            pk_d: *sender.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_commitment,
            note_blinding: note.note_blinding(),
            spend_auth_randomizer,
            ak,
            nk,
        };

        let rk: VerificationKey<SpendAuth> = rsk.into();
        let nf = nk.derive_nullifier(0.into(), &note_commitment);
        assert!(proof
            .verify(anchor, value_to_send.commit(Fr::rand(&mut rng)), nf, rk)
            .is_err());
    }

    #[test]
    fn test_spend_proof_verification_nullifier_integrity_failure() {
        let mut rng = OsRng;
        let seed_phrase = SeedPhrase::generate(&mut rng);
        let spend_seed = SpendSeed::from_seed_phrase(seed_phrase, 0);
        let sk_sender = SpendKey::new(spend_seed);
        let fvk_sender = sk_sender.full_viewing_key();
        let ivk_sender = fvk_sender.incoming();
        let (sender, _dtk_d) = ivk_sender.payment_address(0u64.into());

        let value_to_send = Value {
            amount: 10,
            asset_id: asset::REGISTRY.parse_denom("upenumbra").unwrap().id(),
        };
        let v_blinding = Fr::rand(&mut rng);
        let note = Note::generate(&mut rng, &sender, value_to_send);
        let note_commitment = note.commit();
        let spend_auth_randomizer = Fr::rand(&mut rng);
        let rsk = sk_sender.spend_auth_key().randomize(&spend_auth_randomizer);
        let nk = *sk_sender.nullifier_key();
        let ak = sk_sender.spend_auth_key().into();
        let mut nct = merkle::BridgeTree::<note::Commitment, 32>::new(5);
        nct.append(&note_commitment);
        nct.witness();
        let anchor = nct.root2();
        let merkle_path = nct.authentication_path(&note_commitment).unwrap();

        let proof = SpendProof {
            merkle_path,
            position: 0.into(),
            g_d: *sender.diversified_generator(),
            pk_d: *sender.transmission_key(),
            value: value_to_send,
            v_blinding,
            note_commitment,
            note_blinding: note.note_blinding(),
            spend_auth_randomizer,
            ak,
            nk,
        };

        let rk: VerificationKey<SpendAuth> = rsk.into();
        let incorrect_nf = nk.derive_nullifier(5.into(), &note_commitment);
        assert!(proof
            .verify(anchor, value_to_send.commit(v_blinding), incorrect_nf, rk)
            .is_err());
    }
}
