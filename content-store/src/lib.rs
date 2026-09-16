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
//! ## The store owns a STRONG digest (SHA-256), not theater's SHA-1
//!
//! This layer IS the fleet's supply-chain-integrity layer: a box must be able to
//! trust "this hash == these exact bytes" before it loads a wasm into the spine.
//! theater's `store` handler content-addresses with SHA-1, which is
//! collision-broken (SHAttered, 2017) — unacceptable for an integrity address. So
//! the store computes its **own SHA-256** as the public content-address and uses
//! `theater:simple/store` underneath purely as an **opaque byte sink** (its
//! internal SHA-1 ref is its own business and is never exposed). No theater change,
//! no fleet-wide store migration — the store just owns its digest.
//!
//! Mechanically: `put` stores the bytes under a theater **label** whose name is the
//! SHA-256 hex, and `get` resolves that label, fetches the bytes, and
//! **re-verifies** the SHA-256 before returning them (digest-verified by
//! construction). The primitive:
//!
//! * `put(bytes)   -> sha256`  — store content, return its SHA-256 content ref
//! * `get(sha256)  -> bytes`   — retrieve + integrity-verify content by hash
//! * `has(sha256)  -> bool`    — does this hash exist locally?
//!
//! ## Proving it (this actor's `init`)
//!
//! There is nothing to consensus-check, so the CAS is provable *today*, with no
//! mesh. On init we run a self-test against the real host store and assert the
//! contract, then shut down with a pass/fail marker (the store-test harness
//! pattern). put->hash (SHA-256), get round-trips + verifies, has, and dedup
//! (identical bytes -> same hash -> one stored object) map 1:1 to the tests below.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use packr_guest::{export, import, pack_types, Value, ValueType};
use sha2::{Digest, Sha256};

packr_guest::setup_guest!();

// Interface metadata embedded as `__pack_types` — theater verifies every actor
// carries this at setup (it is how exports are discovered and interface hashes
// checked). Declare the host functions we import (a subset of an interface is
// fine) plus the `actor.init` export. Signatures mirror theater's runtime.pact /
// store.pact exactly (a theater content-ref is a hash `string`).
pack_types! {
    imports {
        theater:simple/runtime {
            log: func(msg: string),
            shutdown: func(data: option<list<u8>>) -> result<_, string>,
        }
        theater:simple/store {
            new: func() -> result<string, string>,
            store-at-label: func(store-id: string, label: string, content: list<u8>) -> result<string, string>,
            get: func(store-id: string, content-ref: string) -> result<list<u8>, string>,
            get-by-label: func(store-id: string, label: string) -> result<option<string>, string>,
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

// Store content under a label (the label is our SHA-256 address). Returns
// theater's own internal ref, which we treat as opaque and discard.
#[import(module = "theater:simple/store", name = "store-at-label")]
fn store_at_label(store_id: String, label: String, content: Vec<u8>) -> Result<String, String>;

// Fetch bytes by theater's own ref (obtained via the label lookup).
#[import(module = "theater:simple/store", name = "get")]
fn store_get(store_id: String, content_ref: String) -> Result<Vec<u8>, String>;

// Resolve our SHA-256 label to theater's internal ref (None if absent).
#[import(module = "theater:simple/store", name = "get-by-label")]
fn store_get_by_label(store_id: String, label: String) -> Result<Option<String>, String>;

#[import(module = "theater:simple/store", name = "calculate-total-size")]
fn store_calculate_size(store_id: String) -> Result<u64, String>;

// ============================================================================
// The CAS primitive — SHA-256 addressed, theater store as opaque byte sink
// ============================================================================
//
// A `Cas` binds a theater store instance and exposes put/get/has keyed by the
// store's OWN SHA-256 digest. This is the reusable seat the fetch-by-hash /
// durability layer will drive later: a box missing a hash fetches the bytes from a
// peer, verifies them against the hash, and `put`s them here; the index (Layer 1)
// points names at these SHA-256 hashes.

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

    /// Store `bytes`, returning their SHA-256 content hash (64 lowercase hex).
    /// Idempotent: identical bytes always yield the same hash and are stored once
    /// (theater dedups the underlying bytes; the label is content-derived).
    fn put(&self, bytes: Vec<u8>) -> Result<String, String> {
        let hash = sha256_hex(&bytes);
        // theater's returned ref is its internal SHA-1 address — opaque to us.
        let _theater_ref = store_at_label(self.store_id.clone(), hash.clone(), bytes)?;
        Ok(hash)
    }

    /// Retrieve the bytes for `hash`, verifying they hash back to it. Returns an
    /// error if the hash is absent or the stored bytes fail the digest check.
    fn get(&self, hash: &str) -> Result<Vec<u8>, String> {
        let theater_ref = store_get_by_label(self.store_id.clone(), String::from(hash))?
            .ok_or_else(|| format!("no content for hash {}", hash))?;
        let bytes = store_get(self.store_id.clone(), theater_ref)?;
        // Digest-verify: the whole point of an integrity layer.
        let actual = sha256_hex(&bytes);
        if actual != hash {
            return Err(format!(
                "integrity violation: content for {} hashes to {}",
                hash, actual
            ));
        }
        Ok(bytes)
    }

    /// Does `hash` exist in this store?
    fn has(&self, hash: &str) -> Result<bool, String> {
        Ok(store_get_by_label(self.store_id.clone(), String::from(hash))?.is_some())
    }

    /// Total on-disk size of stored content (post-deduplication). Used by the
    /// self-test to prove dedup stores identical bytes only once.
    fn total_size(&self) -> Result<u64, String> {
        store_calculate_size(self.store_id.clone())
    }
}

/// SHA-256 of `bytes` as 64 lowercase hex chars — the store's public address.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(digest.len() * 2);
    for &b in digest.iter() {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

// ============================================================================
// Self-test — proves the CAS contract against the real host store
// ============================================================================

fn run_tests() -> Result<(), String> {
    log(String::from("=== content-store CAS self-test (SHA-256) ==="));

    let cas = Cas::open()?;

    // --- put -> hash (SHA-256) --------------------------------------------
    let a = b"Hello, the store!".to_vec();
    let hash_a = cas.put(a.clone())?;
    log(format!("put(A) -> {} ({} hex chars)", hash_a, hash_a.len()));
    // The store's public address is a SHA-256 digest: 64 lowercase hex chars,
    // independent of theater's internal SHA-1.
    if hash_a.len() != 64
        || !hash_a
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(format!("expected a 64-char lowercase-hex SHA-256 hash, got {:?}", hash_a));
    }

    // --- get round-trips AND integrity-verifies ---------------------------
    let got = cas.get(&hash_a)?;
    if got != a {
        return Err(String::from("get(hash_a) did not round-trip the bytes"));
    }
    log(String::from("get(hash_a) round-trips + verifies: OK"));

    // --- has ---------------------------------------------------------------
    if !cas.has(&hash_a)? {
        return Err(String::from("has(hash_a) should be true"));
    }
    // A well-formed but absent SHA-256 hash must report false.
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
