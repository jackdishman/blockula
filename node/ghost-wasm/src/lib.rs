//! # Ghost Mode for wallets, as WebAssembly (NOT AUDITED)
//!
//! `blockula-ghost` behind `wasm-bindgen`: what a wallet in a browser or in Node needs to hold a
//! Ghost Mode key, find its notes and prove an Action (docs/GHOST-MODE.md sections 7 and 8).
//! `@blockula/sdk/ghost` is its caller. Nothing here is new cryptography: keys, notes, the tree,
//! note encryption and the prover are the crate's own, so a proof made here is one the node's
//! verifier accepts, byte for byte the same format.
//!
//! **Secrets stay on this side.** JavaScript holds opaque handles:
//!
//! | Handle | Holds | JavaScript gets |
//! |---|---|---|
//! | [`GhostKey`] | the spending key | the address, its fingerprint, and a viewing key only when asked by name |
//! | [`GhostScanner`] | the expanded ML-KEM key and `nk` | for an output that is the wallet's: its value, memo and nullifier |
//! | [`GhostTree`] | the commitment tree's nodes (public) | the size, root and leaves |
//! | [`GhostAction`] | the witness: `sk`, notes, paths, note seeds | the bundle's public parts and the proof |
//!
//! A note's `rseed` never crosses: a spend names its note by position and ciphertext (both public,
//! from the chain), and the note is decrypted again in here. `free()` drops a handle, and the
//! crate wipes the key bytes it can on drop (`zeroize`: the seed, the viewing keys, the expanded
//! ML-KEM key). What it can't wipe is in docs/GHOST-MODE.md section 12 (field-element secrets, the
//! prover's trace), and a WebAssembly memory is one flat buffer the page's own script can read,
//! so this protects against accidents (logs, serialization), not against code running in the page.
//!
//! **Randomness** is the platform's (`crypto.getRandomValues`, through `getrandom`'s `wasm_js`
//! backend): note seeds, encapsulation, dummies and the proof's masks.

use blockula_ghost::circuit::check_witness;
use blockula_ghost::encryption::{ENC_CIPHERTEXT_BYTES, MEMO_BYTES};
use blockula_ghost::field::{Digest, digest_from_bytes, digest_to_bytes};
use blockula_ghost::{
    ActionWitness, Address, CIRCUIT_ID, EncryptedNote, FullTree, MAX_PROOF_BYTES, Note,
    PublicInputs, SpendWitness, SpendingKey, TrialDecryptor, ValueBalance, dummy_output,
    encrypt_note, nullifier, prove, verify,
};
use wasm_bindgen::prelude::*;
use zeroize::Zeroize;

/// A refusal, in a sentence. It never holds key or note material.
type Refusal = String;

/// OS randomness for note seeds, encapsulation and dummies (the prover draws its own masks).
struct OsRng;

impl rand_core::TryRng for OsRng {
    type Error = core::convert::Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0; 4];
        self.try_fill_bytes(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0; 8];
        self.try_fill_bytes(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        // Without randomness nothing here is safe to make: stop (a trap in WebAssembly).
        getrandom::fill(dst).expect("the platform's randomness (crypto.getRandomValues)");
        Ok(())
    }
}

impl rand_core::TryCryptoRng for OsRng {}

fn digest(bytes: &[u8], what: &str) -> Result<Digest, Refusal> {
    let b: &[u8; 32] = bytes
        .try_into()
        .map_err(|_| format!("{what} must be 32 bytes"))?;
    digest_from_bytes(b).ok_or_else(|| format!("{what} isn't a canonical digest"))
}

fn sighash32(bytes: &[u8]) -> Result<[u8; 32], Refusal> {
    bytes
        .try_into()
        .map_err(|_| "the sighash must be 32 bytes".to_string())
}

fn address(bytes: &[u8]) -> Result<Address, Refusal> {
    Address::from_bytes(bytes).ok_or_else(|| "that isn't a Ghost Mode address (1,216 bytes: a canonical owner and a valid ML-KEM-768 key)".to_string())
}

// ----------------------------------------------------------------------------------------- keys

/// A Ghost Mode spending key. Opaque: it has no method that returns the key.
#[wasm_bindgen]
pub struct GhostKey {
    key: SpendingKey,
}

impl GhostKey {
    /// `SpendingKey::from_master(master, account)`: the same key the CLI derives from a wallet
    /// seed (`blockula ghost`, account 0). The master seed's copy in here is wiped.
    pub fn derive(mut master: Vec<u8>, account: u32) -> Result<GhostKey, Refusal> {
        let seed: Result<[u8; 32], _> = master.as_slice().try_into();
        master.zeroize();
        let mut seed = seed.map_err(|_| "the master seed must be 32 bytes".to_string())?;
        let key = SpendingKey::from_master(&seed, account);
        seed.zeroize();
        Ok(GhostKey { key })
    }

    fn viewing(&self, which: &str) -> Result<Vec<u8>, Refusal> {
        let fvk = self.key.full_viewing_key();
        match which {
            "full" => Ok(fvk.to_bytes().to_vec()),
            "incoming" => Ok(fvk.incoming().to_bytes().to_vec()),
            "outgoing" => Ok(fvk.outgoing().0.to_vec()),
            _ => Err("a viewing key is \"full\", \"incoming\" or \"outgoing\"".to_string()),
        }
    }
}

#[wasm_bindgen]
impl GhostKey {
    /// The `account`-th Ghost Mode key of a 32-byte master seed.
    #[wasm_bindgen(js_name = fromMaster)]
    pub fn from_master(master: Vec<u8>, account: u32) -> Result<GhostKey, JsError> {
        Self::derive(master, account).map_err(|e| JsError::new(&e))
    }

    /// The address: `owner ‖ ek`, 1,216 bytes. Public.
    pub fn address(&self) -> Vec<u8> {
        self.key.address().to_bytes().to_vec()
    }

    /// The address's fingerprint, `xxxx-xxxx-xxxx-xxxx-xxxx`: what people compare.
    pub fn fingerprint(&self) -> String {
        self.key.address().fingerprint()
    }

    /// The address's owner digest (32 bytes). Public: it's the address's first 32 bytes.
    pub fn owner(&self) -> Vec<u8> {
        digest_to_bytes(&self.key.address().owner()).to_vec()
    }

    /// A viewing key's bytes: `"full"` (161 bytes: everything received and sent, and what's
    /// spent), `"incoming"` (97) or `"outgoing"` (32). **Whoever holds it sees that, for good.**
    /// None of them can spend.
    #[wasm_bindgen(js_name = exportViewingKey)]
    pub fn export_viewing_key(&self, which: &str) -> Result<Vec<u8>, JsError> {
        self.viewing(which).map_err(|e| JsError::new(&e))
    }
}

/// The fingerprint of an address someone published. Refuses bytes that aren't an address.
#[wasm_bindgen(js_name = addressFingerprint)]
pub fn address_fingerprint(bytes: &[u8]) -> Result<String, JsError> {
    address(bytes)
        .map(|a| a.fingerprint())
        .map_err(|e| JsError::new(&e))
}

// ----------------------------------------------------------------------------------------- tree

/// The commitment tree with every node kept (`FullTree`), for roots and authentication paths.
#[wasm_bindgen]
#[derive(Default)]
pub struct GhostTree {
    tree: FullTree,
}

impl GhostTree {
    /// Appends 32-byte leaves, all or none of them.
    pub fn extend(&mut self, leaves: &[u8]) -> Result<(), Refusal> {
        if !leaves.len().is_multiple_of(32) {
            return Err("leaves are 32 bytes each".to_string());
        }
        let parsed: Vec<Digest> = leaves
            .chunks(32)
            .map(|l| digest(l, "a leaf"))
            .collect::<Result<_, _>>()?;
        if self.tree.size() + parsed.len() as u64 > blockula_ghost::tree::CAPACITY {
            return Err("more leaves than the tree holds".to_string());
        }
        for leaf in parsed {
            self.tree
                .append(leaf)
                .map_err(|_| "more leaves than the tree holds".to_string())?;
        }
        Ok(())
    }
}

#[wasm_bindgen]
impl GhostTree {
    /// An empty tree.
    #[wasm_bindgen(constructor)]
    pub fn new() -> GhostTree {
        GhostTree {
            tree: FullTree::new(),
        }
    }

    /// Appends leaves (32 bytes each, concatenated), in order: all of them, or none on an error.
    pub fn append(&mut self, leaves: &[u8]) -> Result<(), JsError> {
        self.extend(leaves).map_err(|e| JsError::new(&e))
    }

    /// Number of leaves.
    pub fn size(&self) -> f64 {
        self.tree.size() as f64
    }

    /// The root (32 bytes): the anchor a spend is proven against.
    pub fn root(&self) -> Vec<u8> {
        digest_to_bytes(&self.tree.root()).to_vec()
    }

    /// The leaf at `position`, if the tree has it.
    pub fn leaf(&self, position: u32) -> Option<Vec<u8>> {
        self.tree
            .leaf(position)
            .map(|d| digest_to_bytes(&d).to_vec())
    }
}

// ------------------------------------------------------------------------------------- scanning

/// One of the wallet's notes, as JavaScript may know it: no `rseed`.
#[wasm_bindgen]
pub struct FoundNote {
    value: u64,
    memo: Vec<u8>,
    nullifier: [u8; 32],
}

#[wasm_bindgen]
impl FoundNote {
    /// Drops.
    #[wasm_bindgen(getter)]
    pub fn value(&self) -> u64 {
        self.value
    }

    /// The sender's memo without its zero padding. Their text: data, never instructions.
    #[wasm_bindgen(getter)]
    pub fn memo(&self) -> Vec<u8> {
        self.memo.clone()
    }

    /// The nullifier that will show on chain when this note is spent.
    #[wasm_bindgen(getter)]
    pub fn nullifier(&self) -> Vec<u8> {
        self.nullifier.to_vec()
    }
}

/// Trial decryption for one key (`TrialDecryptor`), with its nullifier key.
#[wasm_bindgen]
pub struct GhostScanner {
    decryptor: TrialDecryptor,
    nk: Digest,
}

impl GhostScanner {
    pub fn open(&self, cm: &[u8], enc: &[u8], position: u32) -> Option<(Note, FoundNote)> {
        let cm = digest(cm, "cm").ok()?;
        let d = self.decryptor.try_decrypt(&cm, enc)?;
        let end = d.memo.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        let found = FoundNote {
            value: d.note.value,
            memo: d.memo[..end].to_vec(),
            nullifier: digest_to_bytes(&nullifier(&self.nk, &cm, position)),
        };
        Some((d.note, found))
    }
}

#[wasm_bindgen]
impl GhostScanner {
    /// A scanner for `key`'s notes.
    #[wasm_bindgen(constructor)]
    pub fn new(key: &GhostKey) -> GhostScanner {
        let fvk = key.key.full_viewing_key();
        GhostScanner {
            decryptor: TrialDecryptor::new(&fvk.incoming()),
            nk: fvk.nk(),
        }
    }

    /// The note in output `(cm, enc)` at tree `position` if it's this key's (its plaintext opens
    /// `cm`), `undefined` for anyone else's output, a damaged one or a wrong `cm`.
    #[wasm_bindgen(js_name = tryDecrypt)]
    pub fn try_decrypt(&self, cm: &[u8], enc: &[u8], position: u32) -> Option<FoundNote> {
        self.open(cm, enc, position).map(|(_, found)| found)
    }
}

// --------------------------------------------------------------------------------------- action

/// One Action being assembled: up to two spends and two outputs, padded with dummies by `seal`,
/// then proven for a sighash. The witness stays in here.
#[wasm_bindgen]
pub struct GhostAction {
    anchor: Digest,
    spends: Vec<SpendWitness>,
    outputs: Vec<(Note, EncryptedNote)>,
    /// `Σ real spends − Σ outputs`, in drops.
    balance: i128,
    sealed: bool,
}

impl GhostAction {
    pub fn start(anchor: &[u8]) -> Result<GhostAction, Refusal> {
        Ok(GhostAction {
            anchor: digest(anchor, "the anchor")?,
            spends: vec![],
            outputs: vec![],
            balance: 0,
            sealed: false,
        })
    }

    fn open(&self) -> Result<(), Refusal> {
        if self.sealed {
            Err("this action is sealed: nothing can be added".to_string())
        } else {
            Ok(())
        }
    }

    fn done(&self) -> Result<(), Refusal> {
        if self.sealed {
            Ok(())
        } else {
            Err("seal the action first".to_string())
        }
    }

    pub fn add_spend(
        &mut self,
        key: &GhostKey,
        tree: &GhostTree,
        position: u32,
        enc: &[u8],
    ) -> Result<u64, Refusal> {
        self.open()?;
        if self.spends.len() >= 2 {
            return Err("one action spends at most two notes".to_string());
        }
        if enc.len() != ENC_CIPHERTEXT_BYTES {
            return Err(format!("a note ciphertext is {ENC_CIPHERTEXT_BYTES} bytes"));
        }
        let cm = tree
            .tree
            .leaf(position)
            .ok_or_else(|| "that note isn't in the tree".to_string())?;
        // The note is decrypted again here, so its secrets never leave: it's this key's only if
        // its plaintext opens the leaf at that position.
        let fvk = key.key.full_viewing_key();
        let note = TrialDecryptor::new(&fvk.incoming())
            .try_decrypt(&cm, enc)
            .ok_or_else(|| "that output isn't a note of this key".to_string())?
            .note;
        let path = tree
            .tree
            .path(position)
            .ok_or_else(|| "that note isn't in the tree".to_string())?;
        if path.root(&cm) != self.anchor {
            return Err("the tree's root isn't this action's anchor".to_string());
        }
        self.spends.push(SpendWitness::new(&key.key, note, path));
        self.balance += i128::from(note.value);
        Ok(note.value)
    }

    pub fn add_output(
        &mut self,
        from: &GhostKey,
        to: &[u8],
        value: u64,
        memo: &[u8],
    ) -> Result<(), Refusal> {
        self.open()?;
        if self.outputs.len() >= 2 {
            return Err("one action makes at most two notes".to_string());
        }
        if memo.len() > MEMO_BYTES {
            return Err(format!("a memo is at most {MEMO_BYTES} bytes"));
        }
        let to = address(to)?;
        let mut padded = [0u8; MEMO_BYTES];
        padded[..memo.len()].copy_from_slice(memo);
        let mut rseed = [0u8; 32];
        getrandom::fill(&mut rseed)
            .map_err(|_| "the platform has no randomness (crypto.getRandomValues)".to_string())?;
        let note = Note {
            value,
            owner: to.owner(),
            rseed,
        };
        rseed.zeroize();
        // Recoverable later with the sender's outgoing viewing key.
        let enc = encrypt_note(
            &note,
            &to,
            &padded,
            Some(&from.key.full_viewing_key().outgoing()),
            &mut OsRng,
        )
        .map_err(|_| "the note isn't for that address".to_string())?;
        self.outputs.push((note, enc));
        self.balance -= i128::from(value);
        Ok(())
    }

    pub fn pad(&mut self) -> Result<(), Refusal> {
        self.open()?;
        while self.spends.len() < 2 {
            self.spends.push(SpendWitness::dummy(&mut OsRng));
        }
        while self.outputs.len() < 2 {
            self.outputs.push(dummy_output(&mut OsRng));
        }
        self.sealed = true;
        Ok(())
    }

    pub fn witness(&self, sighash: &[u8]) -> Result<ActionWitness, Refusal> {
        self.done()?;
        let spends: [SpendWitness; 2] = [self.spends[0].clone(), self.spends[1].clone()];
        Ok(ActionWitness {
            spends,
            outputs: [self.outputs[0].0, self.outputs[1].0],
            anchor: self.anchor,
            sighash: sighash32(sighash)?,
        })
    }

    /// The statement's public parts and the proof. A witness that doesn't add up is an error that
    /// names a constraint, never a value, and never an invalid proof.
    pub fn make_proof(&self, sighash: &[u8]) -> Result<(PublicInputs, Vec<u8>), Refusal> {
        prove(&self.witness(sighash)?)
            .map_err(|e| format!("the Action proof couldn't be made ({e:?})"))
    }

    fn part(&self, i: u32, what: impl Fn(usize) -> Vec<u8>) -> Result<Vec<u8>, Refusal> {
        self.done()?;
        if i > 1 {
            Err("an action has two of each: 0 or 1".to_string())
        } else {
            Ok(what(i as usize))
        }
    }
}

fn js<T>(r: Result<T, Refusal>) -> Result<T, JsError> {
    r.map_err(|e| JsError::new(&e))
}

#[wasm_bindgen]
impl GhostAction {
    /// An action anchored at `anchor` (the tree root its spends are in; any usable root for one
    /// that spends nothing, like a shield).
    #[wasm_bindgen(constructor)]
    pub fn new(anchor: &[u8]) -> Result<GhostAction, JsError> {
        js(Self::start(anchor))
    }

    /// Spends `key`'s note at `position` of `tree`, whose ciphertext on chain is `enc`. Returns
    /// its value in drops.
    pub fn spend(
        &mut self,
        key: &GhostKey,
        tree: &GhostTree,
        position: u32,
        enc: &[u8],
    ) -> Result<u64, JsError> {
        js(self.add_spend(key, tree, position, enc))
    }

    /// Adds a note of `value` drops for the address `to`, with a memo of up to 256 bytes.
    pub fn output(
        &mut self,
        from: &GhostKey,
        to: &[u8],
        value: u64,
        memo: &[u8],
    ) -> Result<(), JsError> {
        js(self.add_output(from, to, value, memo))
    }

    /// Pads to two spends and two outputs with dummies. After this nothing can be added.
    pub fn seal(&mut self) -> Result<(), JsError> {
        js(self.pad())
    }

    /// `Σ spent − Σ output` in drops, as a decimal string: what leaves the pool (a fee, an
    /// unshield's amount and fee), or `-amount` for a shield.
    #[wasm_bindgen(js_name = valueBalance)]
    pub fn value_balance(&self) -> String {
        self.balance.to_string()
    }

    /// The anchor (32 bytes).
    pub fn anchor(&self) -> Vec<u8> {
        digest_to_bytes(&self.anchor).to_vec()
    }

    /// Spend `i`'s nullifier (32 bytes).
    pub fn nullifier(&self, i: u32) -> Result<Vec<u8>, JsError> {
        js(self.part(i, |i| digest_to_bytes(&self.spends[i].nullifier()).to_vec()))
    }

    /// Output `i`'s note commitment (32 bytes).
    pub fn commitment(&self, i: u32) -> Result<Vec<u8>, JsError> {
        js(self.part(i, |i| {
            digest_to_bytes(&self.outputs[i].0.commitment()).to_vec()
        }))
    }

    /// Output `i`'s ciphertext for its recipient (1,401 bytes).
    pub fn enc(&self, i: u32) -> Result<Vec<u8>, JsError> {
        js(self.part(i, |i| self.outputs[i].1.enc.clone()))
    }

    /// Output `i`'s ciphertext for the sender's outgoing viewing key (80 bytes).
    pub fn out(&self, i: u32) -> Result<Vec<u8>, JsError> {
        js(self.part(i, |i| self.outputs[i].1.out.clone()))
    }

    /// Checks the witness against every constraint for `sighash`, without proving.
    pub fn check(&self, sighash: &[u8]) -> Result<(), JsError> {
        js(self.witness(sighash).and_then(|w| {
            check_witness(&w)
                .map(|_| ())
                .map_err(|e| format!("the action doesn't add up ({e:?})"))
        }))
    }

    /// Proves the action for `sighash` (the transaction's `ghostSighash`) and returns the proof
    /// bytes, as the node verifies them. The constraints are checked first.
    pub fn prove(&self, sighash: &[u8]) -> Result<Vec<u8>, JsError> {
        js(self.make_proof(sighash).map(|(_, proof)| proof))
    }
}

/// Verifies an Action proof against its statement, as a node does. `value_balance` is drops, a
/// signed decimal string.
#[wasm_bindgen(js_name = verifyAction)]
#[allow(clippy::too_many_arguments)]
pub fn verify_action(
    anchor: &[u8],
    nf0: &[u8],
    nf1: &[u8],
    cm0: &[u8],
    cm1: &[u8],
    value_balance: &str,
    sighash: &[u8],
    proof: &[u8],
) -> Result<bool, JsError> {
    js((|| {
        let balance: i128 = value_balance
            .parse()
            .map_err(|_| "the value balance is a whole number of drops".to_string())?;
        let public = PublicInputs {
            anchor: digest(anchor, "the anchor")?,
            nullifiers: [digest(nf0, "a nullifier")?, digest(nf1, "a nullifier")?],
            commitments: [digest(cm0, "a commitment")?, digest(cm1, "a commitment")?],
            value_balance: ValueBalance::new(balance)
                .ok_or_else(|| "the value balance is out of range".to_string())?,
            sighash: sighash32(sighash)?,
        };
        Ok(verify(&public, proof).is_ok())
    })())
}

/// The Action circuit this build proves (`CIRCUIT_ID`): a node must verify the same one.
#[wasm_bindgen(js_name = circuitId)]
pub fn circuit_id() -> u32 {
    CIRCUIT_ID
}

/// The largest proof a node accepts, in bytes.
#[wasm_bindgen(js_name = maxProofBytes)]
pub fn max_proof_bytes() -> u32 {
    MAX_PROOF_BYTES as u32
}

#[cfg(test)]
mod tests;
