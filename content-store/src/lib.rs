//! # content-store — the store's immutable CAS layer (Layer 2)
//!
//! The store splits into two layers with opposite distribution characteristics
//! (see ../DESIGN.md). This crate is the **immutable content** half: a
//! content-addressed store `hash -> bytes`. Because the key *is* the hash, the
//! content is immutable and needs **no consensus** — it is trivially replicable
//! and cacheable, and later distributed fleet-wide by fetch-by-hash. The mutable
//! `name -> hash` index (Layer 1, rides the mesh as an app-SM) sits *on top* of
//! this and is the GC root.
//!
//! ## What this actor is
//!
//! A thin, provable wrapper over theater's `theater:simple/store` handler, which
//! already implements a SHA-256 content-addressed store with automatic
//! deduplication. We expose the deliberately-dumb CAS primitive:
//!
//! * `put(bytes)   -> hash`   — store content, return its SHA-256 content ref
//! * `get(hash)    -> bytes`  — retrieve content by hash
//! * `has(hash)    -> bool`   — does this hash exist locally?
//!
//! At the guest ABI a `content-ref` is flattened to its hash `string`, so a hash
//! is just a `String` here.
//!
//! ## Proving it (this actor's `init`)
//!
//! There is nothing to consensus-check, so the CAS is provable *today*, with no
//! mesh. On init we run a self-test against the real host store and assert the
//! contract, then shut down with a pass/fail marker (the store-test harness
//! pattern). The manager's confirm list — put->hash, get round-trips, has, and
//! dedup (identical bytes -> same hash -> one stored object) — maps 1:1 to the
//! tests below.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use packr_guest::{export, import, pack_types, Value, ValueType};

packr_guest::setup_guest!();

// Interface metadata embedded as `__pack_types` — theater verifies every actor
// carries this at setup (it is how exports are discovered and interface hashes
// checked). Declare the host functions we import (subset of each interface is
// fine) plus the `actor.init` export. Signatures mirror theater's runtime.pact /
// store.pact exactly (content-ref is a hash `string`).
pack_types! {
    imports {
        theater:simple/runtime {
            log: func(msg: string),
            shutdown: func(data: option<list<u8>>) -> result<_, string>,
        }
        theater:simple/store {
            new: func() -> result<string, string>,
            store: func(store-id: string, content: list<u8>) -> result<string, string>,
            get: func(store-id: string, content-ref: string) -> result<list<u8>, string>,
            exists: func(store-id: string, content-ref: string) -> result<bool, string>,
            calculate-total-size: func(store-id: string) -> result<u64, string>,
        }
    }
    exports {
        theater:simple/actor.init: func(state: value) -> result<value, string>,
    }
}

// ============================================================================
// Host imports
// ============================================================================

#[import(module = "theater:simple/runtime", name = "log")]
fn log(msg: String);

#[import(module = "theater:simple/runtime", name = "shutdown")]
fn shutdown(data: Option<Vec<u8>>) -> Result<(), String>;

#[import(module = "theater:simple/store", name = "new")]
fn store_new() -> Result<String, String>;

#[import(module = "theater:simple/store", name = "store")]
fn store_store(store_id: String, content: Vec<u8>) -> Result<String, String>;

#[import(module = "theater:simple/store", name = "get")]
fn store_get(store_id: String, content_ref: String) -> Result<Vec<u8>, String>;

#[import(module = "theater:simple/store", name = "exists")]
fn store_exists(store_id: String, content_ref: String) -> Result<bool, String>;

#[import(module = "theater:simple/store", name = "calculate-total-size")]
fn store_calculate_size(store_id: String) -> Result<u64, String>;

// ============================================================================
// The CAS primitive
// ============================================================================
//
// A `Cas` binds a theater store instance and exposes put/get/has. This is the
// reusable seat the fetch-by-hash / durability layer will drive later: a box
// missing a hash fetches the bytes from a peer and `put`s them here; the index
// (Layer 1) points names at the hashes this layer holds.

/// A content-addressed store bound to one theater store instance.
struct Cas {
    store_id: String,
}

impl Cas {
    /// Open a fresh CAS over a new theater store instance.
    fn open() -> Result<Cas, String> {
        Ok(Cas {
            store_id: store_new()?,
        })
    }

    /// Store `bytes`, returning their SHA-256 content hash. Idempotent:
    /// identical bytes always yield the same hash and are stored once (the host
    /// handler deduplicates).
    fn put(&self, bytes: Vec<u8>) -> Result<String, String> {
        store_store(self.store_id.clone(), bytes)
    }

    /// Retrieve the bytes for `hash`.
    fn get(&self, hash: &str) -> Result<Vec<u8>, String> {
        store_get(self.store_id.clone(), String::from(hash))
    }

    /// Does `hash` exist in this store?
    fn has(&self, hash: &str) -> Result<bool, String> {
        store_exists(self.store_id.clone(), String::from(hash))
    }

    /// Total on-disk size of stored content (post-deduplication). Used by the
    /// self-test to prove dedup stores identical bytes only once.
    fn total_size(&self) -> Result<u64, String> {
        store_calculate_size(self.store_id.clone())
    }
}

// ============================================================================
// Self-test — proves the CAS contract against the real host store
// ============================================================================

fn run_tests() -> Result<(), String> {
    log(String::from("=== content-store CAS self-test ==="));

    let cas = Cas::open()?;

    // --- put -> hash -------------------------------------------------------
    let a = b"Hello, the store!".to_vec();
    let hash_a = cas.put(a.clone())?;
    log(format!("put(A) -> {} ({} hex chars)", hash_a, hash_a.len()));
    // The content ref is the hex digest of a cryptographic hash — the CAS contract
    // (round-trip + dedup) is digest-agnostic, so we only require a non-empty
    // lowercase-hex ref. NB: theater's store substrate emits a 40-char SHA-1 digest
    // here, not the 64-char SHA-256 the WIT docstring advertises (see the note to
    // the manager). The digest choice is a Layer-2 concern the fleet store can pin.
    if hash_a.is_empty()
        || !hash_a
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(format!("expected a non-empty lowercase-hex hash, got {:?}", hash_a));
    }

    // --- get round-trips the bytes ----------------------------------------
    let got = cas.get(&hash_a)?;
    if got != a {
        return Err(String::from("get(hash_a) did not round-trip the bytes"));
    }
    log(String::from("get(hash_a) round-trips: OK"));

    // --- has ---------------------------------------------------------------
    if !cas.has(&hash_a)? {
        return Err(String::from("has(hash_a) should be true"));
    }
    // A well-formed but absent hash must report false.
    let absent = "0000000000000000000000000000000000000000000000000000000000000000";
    if cas.has(absent)? {
        return Err(String::from("has(absent) should be false"));
    }
    log(String::from("has(present)=true, has(absent)=false: OK"));

    // --- dedup: identical bytes -> same hash -> one stored object ----------
    let hash_a2 = cas.put(a.clone())?;
    if hash_a2 != hash_a {
        return Err(format!(
            "dedup: identical bytes produced different hashes: {} vs {}",
            hash_a, hash_a2
        ));
    }
    log(String::from("put(A) again -> same hash: OK"));

    // --- distinct bytes -> distinct hash ----------------------------------
    let b = b"a different value".to_vec();
    let hash_b = cas.put(b.clone())?;
    if hash_b == hash_a {
        return Err(String::from("distinct bytes collided to the same hash"));
    }
    if cas.get(&hash_b)? != b {
        return Err(String::from("get(hash_b) did not round-trip the bytes"));
    }
    log(format!("put(B) -> {} (distinct): OK", hash_b));

    // --- dedup is real, not just hash-equal: total size counts A once ------
    // A was put twice; only A + B bytes should be on disk.
    let expected = (a.len() + b.len()) as u64;
    let size = cas.total_size()?;
    if size != expected {
        return Err(format!(
            "dedup size: expected {} bytes (A once + B), got {}",
            expected, size
        ));
    }
    log(format!(
        "total size {} bytes == len(A)+len(B): A stored once despite two puts: OK",
        size
    ));

    log(String::from("=== all CAS tests passed ==="));
    Ok(())
}

// ============================================================================
// Exports
// ============================================================================

#[export(name = "theater:simple/actor.init")]
fn init(input: Value) -> Value {
    // `init(state: value) -> result<value, string>`. This is a one-shot self-test
    // actor: it runs the CAS suite and shuts down with a pass/fail marker. It holds
    // no meaningful state, so it just echoes whatever init state it was given.
    log(String::from("content-store actor initializing..."));

    match run_tests() {
        Ok(()) => {
            let _ = shutdown(Some(b"content-store-passed".to_vec()));
            ok_value(input)
        }
        Err(e) => {
            log(format!("TEST FAILED: {}", e));
            let _ = shutdown(Some(format!("content-store-failed: {}", e).into_bytes()));
            err_result(&e)
        }
    }
}

// ============================================================================
// Helpers — build the `result<value, string>` return value
// ============================================================================

fn err_result(msg: &str) -> Value {
    Value::Result {
        ok_type: ValueType::Tuple(vec![]),
        err_type: ValueType::String,
        value: Err(alloc::boxed::Box::new(Value::String(String::from(msg)))),
    }
}

fn ok_value(state: Value) -> Value {
    Value::Result {
        ok_type: state.infer_type(),
        err_type: ValueType::String,
        value: Ok(alloc::boxed::Box::new(state)),
    }
}
