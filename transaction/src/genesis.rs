use ark_ff::{One, Zero};
use penumbra_crypto::{
    ka,
    memo::{MemoCiphertext, MEMO_CIPHERTEXT_LEN_BYTES},
    merkle,
    note::OVK_WRAPPED_LEN_BYTES,
    Fr, Note,
};

use crate::{
    action::{output, Output},
    Action, Error, Fee, Transaction, TransactionBody,
};

/// Used to construct a Penumbra transaction from genesis notes.
///
/// When genesis notes are created, we construct a single genesis transaction
/// for them such that all transactions (genesis and non-genesis) can be
/// treated equally by clients.
///
/// The `GenesisBuilder` has no way to create spends, only outputs, and
/// allows for a non-zero value balance.
///
/// While the genesis transaction has the same form as normal transactions,
/// its outputs are not private, and the `GenesisBuilder` uses constant values
/// instead of RNG outputs.
pub struct GenesisBuilder {
    // Actions we'll perform in this transaction.
    pub actions: Vec<Action>,
    // Transaction fee. None if unset.
    pub fee: Option<Fee>,
    // Sum of blinding factors for each value commitment.
    pub synthetic_blinding_factor: Fr,
    // Sum of value commitments.
    pub value_commitments: decaf377::Element,
    // Value balance.
    pub value_balance: decaf377::Element,
    // The root of the note commitment merkle tree.
    pub merkle_root: merkle::Root,
    // Expiry height. None if unset.
    pub expiry_height: Option<u32>,
    // Chain ID. None if unset.
    pub chain_id: Option<String>,
}

impl GenesisBuilder {
    /// Create a new `Output` for the genesis note.
    ///
    /// This output is not private!
    pub fn add_output(&mut self, note: Note) {
        let v_blinding = Fr::zero();
        // We subtract from the transaction's value balance.
        self.synthetic_blinding_factor -= v_blinding;
        self.value_balance -= Fr::from(note.amount()) * note.asset_id().value_generator();

        // Use the secret key "1" in case we decide we want contributory
        // behaviour for `decaf377-ka` later
        let esk = ka::Secret::new_from_field(Fr::one());
        let body = output::Body::new(
            note.clone(),
            v_blinding,
            note.diversified_generator(),
            note.transmission_key(),
            &esk,
        );
        self.value_commitments += body.value_commitment.0;

        // xx Hardcore something in the memo for genesis?
        // let encrypted_memo = memo.encrypt(&esk, &dest);
        let encrypted_memo = MemoCiphertext([0u8; MEMO_CIPHERTEXT_LEN_BYTES]);

        // In the case of genesis notes, the notes are transparent, so we fill
        // the `ovk_wrapped_key` field with 0s.
        let ovk_wrapped_key = [0u8; OVK_WRAPPED_LEN_BYTES];

        let output = Action::Output(Output {
            body,
            encrypted_memo,
            ovk_wrapped_key,
        });
        self.actions.push(output);
    }

    /// Set the chain ID.
    pub fn set_chain_id(mut self, chain_id: String) -> Self {
        self.chain_id = Some(chain_id);
        self
    }

    pub fn finalize(self) -> Result<Transaction, Error> {
        if self.chain_id.is_none() {
            return Err(Error::NoChainID);
        }

        let transaction_body = TransactionBody {
            merkle_root: self.merkle_root.clone(),
            actions: self.actions.clone(),
            expiry_height: 0,
            chain_id: self.chain_id.unwrap(),
            fee: Fee(0),
        };

        let binding_sig = [0u8; 64].into();

        Ok(Transaction {
            transaction_body,
            binding_sig,
        })
    }
}
