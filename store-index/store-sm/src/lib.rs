//! `store-sm` — the store's mutable `name → hash` index as an RSM `state-machine`
//! component (see ../../LAYER0-DESIGN.md §1). Composed into the mesh node via
//! `mesh.lib.mkComposite { name = "store"; sm = store_sm.wasm }`, exactly like `bank-sm`.
//!
//! The index is a CRDT **LWW register per name**. Writes are gated by an allow-list of
//! authorized **node** pubkeys (the node's own key signs each event — the write-auth
//! hook), seeded by a distinguished `Genesis` event. Concurrency converges by the total
//! `(ts, author)` order stored in each entry, so the fold is order-independent.
//!
//! Payload and state are TYPED (`store_protocol::{Cmd, IndexState}`): the node decodes
//! bytes → typed at the fold boundary, so `validate`/`apply` receive real values.

#![cfg_attr(not(test), no_std)]
extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use packr_guest::export;
use store_protocol::{Cmd, IndexState};

#[cfg(not(test))]
packr_guest::setup_guest!();

packr_guest::pack_types! {
    exports {
        state-machine {
            initial-state: func() -> index-state,
            validate: func(id: list<u8>, author: list<u8>, timestamp: u64, payload: cmd, state: index-state) -> result<bool, string>,
            apply: func(id: list<u8>, author: list<u8>, timestamp: u64, payload: cmd, state: index-state) -> index-state,
            members: func(state: index-state) -> list<list<u8>>,
        }
    }
}

// ===== core logic (host-testable) — payload AND state arrive TYPED =====

/// A content hash is a 64-char lowercase-hex SHA-256 (the store's Layer-2 address).
fn is_sha256_hex(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Admission (ancestry-relative, PURE). `author` is the verified signer node pubkey.
fn do_validate(author: &[u8], cmd: &Cmd, state: &IndexState) -> Result<bool, String> {
    match cmd {
        // Genesis is a one-shot bootstrap: admit only while un-initialized. This is the
        // only event not gated on the allow-list (the list does not exist yet).
        Cmd::Genesis { .. } => {
            if state.genesis_done {
                Err("genesis already applied".to_string())
            } else {
                Ok(true)
            }
        }
        Cmd::Put { name, hash } => {
            gate_write(author, state)?;
            if name.is_empty() {
                return Err("empty name".to_string());
            }
            if !is_sha256_hex(hash) {
                return Err("hash must be 64-char lowercase-hex SHA-256".to_string());
            }
            Ok(true)
        }
        Cmd::Remove { name } => {
            gate_write(author, state)?;
            if name.is_empty() {
                return Err("empty name".to_string());
            }
            Ok(true)
        }
    }
}

/// Shared write gate: the store must be initialized and the author allow-listed.
fn gate_write(author: &[u8], state: &IndexState) -> Result<(), String> {
    if !state.genesis_done {
        return Err("store not initialized (no genesis)".to_string());
    }
    if !state.is_authorized(author) {
        return Err("unauthorized author".to_string());
    }
    Ok(())
}

/// Deterministic transition (PURE, called only on events that passed `validate`).
/// Admission-final hands us ALL concurrent writes, so the winner is picked from event
/// CONTENT (`ts, author`) via the LWW register — never from fold position.
fn do_apply(author: Vec<u8>, timestamp: u64, cmd: Cmd, mut state: IndexState) -> IndexState {
    match cmd {
        Cmd::Genesis { mut allow_list } => {
            if !state.genesis_done {
                allow_list.sort();
                allow_list.dedup();
                state.allow_list = allow_list;
                state.genesis_done = true;
            }
        }
        Cmd::Put { name, hash } => {
            state.lww_upsert(name, hash, false, timestamp, author);
        }
        Cmd::Remove { name } => {
            state.lww_upsert(name, String::new(), true, timestamp, author);
        }
    }
    state
}

// ===== the state-machine interface =====

#[export(name = "initial-state")]
fn initial_state() -> IndexState {
    IndexState::default()
}

#[export]
fn validate(_id: Vec<u8>, author: Vec<u8>, _timestamp: u64, payload: Cmd, state: IndexState) -> Result<bool, String> {
    do_validate(&author, &payload, &state)
}

#[export]
fn apply(_id: Vec<u8>, author: Vec<u8>, timestamp: u64, payload: Cmd, state: IndexState) -> IndexState {
    do_apply(author, timestamp, payload, state)
}

/// Project the member set (allow-listed node pubkeys) — feeds the mesh's witness-based
/// finality. Dormant in admission-final v0 but declared for interface-hash stability.
#[export]
fn members(state: IndexState) -> Vec<Vec<u8>> {
    state.allow_list.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn pk(n: u8) -> Vec<u8> {
        vec![n; 32]
    }
    fn genesis(allow: &[u8]) -> Cmd {
        Cmd::Genesis { allow_list: allow.iter().map(|&n| pk(n)).collect() }
    }
    fn put(name: &str, hash: &str) -> Cmd {
        Cmd::Put { name: name.to_string(), hash: hash.to_string() }
    }
    fn h(seed: char) -> String {
        core::iter::repeat(seed).take(64).collect()
    }

    /// Fold honest events (author, ts, cmd), applying only what validates — the node's rule.
    fn fold(seed: IndexState, evs: &[(Vec<u8>, u64, Cmd)]) -> IndexState {
        let mut s = seed;
        for (author, ts, c) in evs {
            if do_validate(author, c, &s).is_ok() {
                s = do_apply(author.clone(), *ts, c.clone(), s);
            }
        }
        s
    }

    #[test]
    fn genesis_then_authorized_put_is_admitted() {
        let s = fold(
            IndexState::default(),
            &[
                (pk(9), 1, genesis(&[1, 2])),
                (pk(1), 10, put("wasm/inbox", &h('a'))),
            ],
        );
        assert!(s.genesis_done);
        assert_eq!(s.resolve("wasm/inbox"), Some(h('a').as_str()));
    }

    #[test]
    fn unauthorized_write_is_rejected() {
        let s = fold(IndexState::default(), &[(pk(9), 1, genesis(&[1, 2]))]);
        // node 3 is not in the allow-list {1,2}
        assert!(do_validate(&pk(3), &put("x", &h('a')), &s).is_err());
        // and folding it changes nothing
        let s2 = fold(s.clone(), &[(pk(3), 5, put("x", &h('a')))]);
        assert_eq!(s2.resolve("x"), None);
    }

    #[test]
    fn write_before_genesis_is_rejected() {
        assert!(do_validate(&pk(1), &put("x", &h('a')), &IndexState::default()).is_err());
    }

    #[test]
    fn malformed_hash_is_rejected() {
        let s = fold(IndexState::default(), &[(pk(9), 1, genesis(&[1]))]);
        assert!(do_validate(&pk(1), &put("x", "not-a-hash"), &s).is_err());
        assert!(do_validate(&pk(1), &put("x", &"A".repeat(64)), &s).is_err(), "uppercase hex rejected");
        assert!(do_validate(&pk(1), &put("x", &h('a')), &s).is_ok());
    }

    #[test]
    fn concurrent_same_name_converges_regardless_of_fold_order() {
        let base = fold(IndexState::default(), &[(pk(9), 1, genesis(&[1, 2]))]);
        // two concurrent Puts to the SAME name, both authorized, distinct (ts, author)
        let w1 = (pk(1), 100u64, put("k", &h('a')));
        let w2 = (pk(2), 200u64, put("k", &h('b'))); // higher ts -> should win
        let order_ab = fold(base.clone(), &[w1.clone(), w2.clone()]);
        let order_ba = fold(base.clone(), &[w2.clone(), w1.clone()]);
        assert_eq!(order_ab, order_ba, "fold order must not change the converged state");
        assert_eq!(order_ab.resolve("k"), Some(h('b').as_str()), "highest (ts,author) wins");
    }

    #[test]
    fn ts_tie_breaks_on_author() {
        let base = fold(IndexState::default(), &[(pk(9), 1, genesis(&[1, 2]))]);
        let w1 = (pk(1), 100u64, put("k", &h('a')));
        let w2 = (pk(2), 100u64, put("k", &h('b'))); // same ts, higher author pk -> wins
        let ab = fold(base.clone(), &[w1.clone(), w2.clone()]);
        let ba = fold(base.clone(), &[w2, w1]);
        assert_eq!(ab, ba);
        assert_eq!(ab.resolve("k"), Some(h('b').as_str()));
    }

    #[test]
    fn remove_is_lww_and_delete_readd_is_deterministic() {
        let base = fold(IndexState::default(), &[(pk(9), 1, genesis(&[1]))]);
        // put@10, remove@20, re-add@30 -> live with the re-add hash, any fold order
        let p = (pk(1), 10u64, put("k", &h('a')));
        let r = (pk(1), 20u64, Cmd::Remove { name: "k".to_string() });
        let p2 = (pk(1), 30u64, put("k", &h('c')));
        let forward = fold(base.clone(), &[p.clone(), r.clone(), p2.clone()]);
        let shuffled = fold(base.clone(), &[p2, r, p]);
        assert_eq!(forward, shuffled);
        assert_eq!(forward.resolve("k"), Some(h('c').as_str()));
        // a remove that wins leaves the name absent
        let removed = fold(base, &[(pk(1), 10, put("k", &h('a'))), (pk(1), 20, Cmd::Remove { name: "k".to_string() })]);
        assert_eq!(removed.resolve("k"), None);
    }

    #[test]
    fn different_names_commute() {
        let base = fold(IndexState::default(), &[(pk(9), 1, genesis(&[1]))]);
        let a = (pk(1), 10u64, put("a", &h('a')));
        let b = (pk(1), 11u64, put("b", &h('b')));
        assert_eq!(fold(base.clone(), &[a.clone(), b.clone()]), fold(base, &[b, a]));
    }

    #[test]
    fn genesis_sorts_and_dedups_allow_list() {
        // members() projects state.allow_list; genesis canonicalizes it (sort + dedup)
        // so every node's folded state is byte-identical.
        let s = fold(IndexState::default(), &[(pk(9), 1, genesis(&[2, 1, 2]))]);
        assert_eq!(s.allow_list, vec![pk(1), pk(2)]);
    }
}
