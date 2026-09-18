//! `store-protocol` — the mutable index schema: the TYPED payload AND the TYPED state
//! for the store's Layer-1 index SM (see ../../LAYER0-DESIGN.md §1).
//!
//! Both the payload (`Cmd`) and the state (`IndexState`) are `#[derive(GraphValue)]`
//! types, so they marshal through the Graph ABI automatically (like `bank-protocol`) —
//! the mesh node is generic over the payload `p` and state `s`, decodes bytes → typed at
//! the fold boundary, and hands the SM real values. No hand-rolled cursor, no serde.
//!
//! ## The state is an LWW register per name
//! `IndexState.entries` is a name-sorted `Vec<Entry>` (a canonical `list<record>` on the
//! wire, so every node serializes byte-identically → convergence holds). Each `Entry`
//! carries the `(ts, author)` that last set it — the LWW tiebreaker LIVES in state, which
//! is what makes `apply` a true CRDT LWW register (fold-order-independent) rather than
//! "last-in-fold-order wins". See LAYER0-DESIGN §1.2/§1.4 (mesh-dev's correctness fix).

#![cfg_attr(not(test), no_std)]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use packr_guest::{decode as abi_decode, encode as abi_encode, GraphValue, Value};

/// A node pubkey (32 bytes). The index's write-auth is an allow-list of these.
pub type PubKey = Vec<u8>;

/// The typed payload kinds authored onto the index chain.
#[derive(Debug, Clone, PartialEq, Eq, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub enum Cmd {
    /// The distinguished genesis event (folded via `apply`): seeds the write allow-list.
    /// Per the mesh contract, per-network config arrives as a genesis EVENT, not a
    /// constructor arg. Admitted only while the store is un-initialized.
    Genesis { allow_list: Vec<PubKey> },
    /// Bind `name` → `hash` (a Layer-2 SHA-256 hex content-address).
    Put { name: String, hash: String },
    /// Tombstone `name`.
    Remove { name: String },
    /// Admit a new writer to the allow-list (mutable membership, post-genesis). An existing
    /// allow-listed writer authors this to add another node's pubkey — so a live cluster can
    /// take on a new writer (e.g. a deploy container) WITHOUT a disruptive re-genesis. A pure
    /// set-add: idempotent + commutative, so it's LWW-safe / order-independent.
    AddWriter { pubkey: PubKey },
}

/// One index entry: a name bound to a content hash (or a tombstone), tagged with the
/// `(ts, author)` that last wrote it (the LWW tiebreaker).
#[derive(Debug, Clone, Default, PartialEq, Eq, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct Entry {
    pub name: String,
    /// Content hash (SHA-256 hex); empty when `tombstone` is true.
    pub hash: String,
    /// True = this name has been removed.
    pub tombstone: bool,
    /// Author's wall clock (ms) — the primary LWW key.
    pub ts: u64,
    /// Authoring node pubkey — the second LWW key (breaks a `ts` tie).
    pub author: PubKey,
    /// Event id (32-byte hash) — the THIRD, final LWW key. Globally unique, so the
    /// register is a total order over events: `(ts, author, id)` never ties, and the
    /// winner is fully content-determined rather than leaning on the node's fold-order
    /// tiebreak (which is a swappable substrate detail, not part of the SM contract).
    pub id: Vec<u8>,
}

/// The index SM's typed state.
#[derive(Debug, Clone, Default, PartialEq, Eq, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct IndexState {
    /// Set true once the genesis event has folded (guards writes before init).
    pub genesis_done: bool,
    /// Authorized node pubkeys (sorted, deduped — canonical for byte-identical folds).
    pub allow_list: Vec<PubKey>,
    /// Name-sorted entries (canonical). A live entry has `tombstone == false`.
    pub entries: Vec<Entry>,
}

impl IndexState {
    /// Is `author` allowed to write?
    pub fn is_authorized(&self, author: &[u8]) -> bool {
        self.allow_list.iter().any(|k| k.as_slice() == author)
    }

    /// The current live hash for `name` (None if absent or tombstoned).
    pub fn resolve(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .filter(|e| !e.tombstone)
            .map(|e| e.hash.as_str())
    }

    /// LWW upsert: write `slot` for `name` iff its `(ts, author, id)` strictly beats the
    /// stored entry's (or the name is new). Keeps `entries` name-sorted. `(ts, author, id)`
    /// is a total order over events (id is globally unique), so the register is
    /// commutative + idempotent — fold order cannot change the result, and a full
    /// `(ts, author)` tie is still broken by content (id), not by the node's fold order.
    pub fn lww_upsert(&mut self, name: String, hash: String, tombstone: bool, ts: u64, author: PubKey, id: Vec<u8>) {
        match self.entries.iter_mut().find(|e| e.name == name) {
            Some(e) => {
                if (ts, author.as_slice(), id.as_slice()) > (e.ts, e.author.as_slice(), e.id.as_slice()) {
                    e.hash = hash;
                    e.tombstone = tombstone;
                    e.ts = ts;
                    e.author = author;
                    e.id = id;
                }
            }
            None => {
                let entry = Entry { name, hash, tombstone, ts, author, id };
                // insert keeping name-sorted order (canonical)
                let pos = self
                    .entries
                    .binary_search_by(|e| e.name.cmp(&entry.name))
                    .unwrap_or_else(|p| p);
                self.entries.insert(pos, entry);
            }
        }
    }
}

/// Encode a command via the Graph ABI (what a driver hands the node's `author`).
pub fn encode(cmd: &Cmd) -> Vec<u8> {
    abi_encode(&Value::from(cmd.clone())).unwrap_or_default()
}

/// Decode a payload; `None` on anything not a well-formed `Cmd`.
pub fn decode(payload: &[u8]) -> Option<Cmd> {
    abi_decode(payload).ok().and_then(|v| Cmd::try_from(v).ok())
}

/// Encode the index state (the wire form the node's `current-state` returns).
pub fn encode_state(state: &IndexState) -> Vec<u8> {
    abi_encode(&Value::from(state.clone())).unwrap_or_default()
}

/// Decode index state; `None` on anything not a well-formed `IndexState`.
pub fn decode_state(bytes: &[u8]) -> Option<IndexState> {
    abi_decode(bytes).ok().and_then(|v| IndexState::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn cmd_round_trips() {
        for cmd in [
            Cmd::Genesis { allow_list: vec![vec![1u8; 32], vec![2u8; 32]] },
            Cmd::Put { name: "wasm/inbox".to_string(), hash: "a".repeat(64) },
            Cmd::Remove { name: "wasm/inbox".to_string() },
        ] {
            assert_eq!(decode(&encode(&cmd)), Some(cmd));
        }
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[9, 9, 9]), None);
    }

    #[test]
    fn state_round_trips_and_is_canonical() {
        let mut a = IndexState::default();
        a.lww_upsert("b".to_string(), "1".repeat(64), false, 2, vec![0u8; 32], vec![1]);
        a.lww_upsert("a".to_string(), "2".repeat(64), false, 1, vec![0u8; 32], vec![2]);
        // entries kept name-sorted regardless of insert order
        assert_eq!(a.entries[0].name, "a");
        assert_eq!(decode_state(&encode_state(&a)), Some(a.clone()));
    }
}
