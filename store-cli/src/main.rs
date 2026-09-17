//! `store` -- the deployment-distribution CLI (see ../DEPLOYMENT-DISTRIBUTION.md).
//!
//! Central-authorized PUBLISH of a wasm into the store, and MATERIALIZE on a box: resolve a
//! name via the mutable index, fetch the bytes by hash from a content holder (verified), and
//! write them to a **content-addressed local path** the roster's manifest `package` field
//! references. Boot stays local (a real file on disk); the supervisor stays store-agnostic.
//!
//! Composes the store's proven layers: the index SM (`name->hash`, over the mesh) and the
//! content wire (`REQ_GET`/`BLOB`/`PUSH`, SHA-256-verified). The mesh client protocol is
//! inlined (no test-crate dep) so this builds + runs standalone on any box.
//!
//! Subcommands:
//!   store init      --index A --node-seed S            # author Genesis: allow-list the index node
//!   store publish   --name N --wasm F --holder A --index A
//!   store materialize --name N --holder A --index A --root D [--out P]
//!   store resolve   --name N --index A

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use std::{env, fs, process};

use store_protocol::{decode_state, encode, Cmd};

// ============================ mesh client (inlined) ============================
// The mutable index rides the mesh; a client speaks this TCP framing to the index node's
// listen port: [len:u32 BE][kind:u8][payload]. Verbs used: HELLO/AUTH handshake, SUBMIT
// (node authors+signs the payload), QUERY(Q_STATE) -> the folded index state.
const HELLO: u8 = 0x01;
const AUTH: u8 = 0x02;
const SUBMIT: u8 = 0x11;
const QUERY: u8 = 0x30;
const CHALLENGE: u8 = 0x80;
const ACCEPTED: u8 = 0x81;
const ACK: u8 = 0x91;
const QUERY_REPLY: u8 = 0xa0;
const Q_STATE: u8 = 0;

fn seeded_key(seed: &str) -> SigningKey {
    SigningKey::from_bytes(&Sha256::digest(seed.as_bytes()).into())
}
fn node_pubkey(seed: &str) -> Vec<u8> {
    seeded_key(seed).verifying_key().to_bytes().to_vec()
}
fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() + 1) as u32;
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    out
}
fn read_frame(s: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
    let mut lb = [0u8; 4];
    s.read_exact(&mut lb)?;
    let mut buf = vec![0u8; u32::from_be_bytes(lb) as usize];
    s.read_exact(&mut buf)?;
    Ok((buf[0], buf[1..].to_vec()))
}
fn read_until(s: &mut TcpStream, kind: u8) -> io::Result<Vec<u8>> {
    loop {
        let (k, p) = read_frame(s)?;
        if k == kind {
            return Ok(p);
        }
    }
}

struct Index {
    stream: TcpStream,
}
impl Index {
    fn connect(addr: &str) -> io::Result<Index> {
        let mut s = TcpStream::connect(addr)?;
        s.set_read_timeout(Some(Duration::from_secs(8)))?;
        // membership-permissive handshake -- any identity may connect; the SM gates writes.
        let key = seeded_key("store-cli");
        s.write_all(&frame(HELLO, &key.verifying_key().to_bytes()))?;
        let nonce = read_until(&mut s, CHALLENGE)?;
        s.write_all(&frame(AUTH, &key.sign(&nonce).to_bytes()))?;
        read_until(&mut s, ACCEPTED)?;
        Ok(Index { stream: s })
    }
    /// SUBMIT a payload; the node authors+signs it. Returns the event id, or an error with
    /// the SM's rejection reason.
    fn submit(&mut self, payload: &[u8]) -> io::Result<[u8; 32]> {
        self.stream.write_all(&frame(SUBMIT, payload))?;
        let ack = read_until(&mut self.stream, ACK)?;
        if ack.len() < 33 || ack[32] != 1 {
            return Err(ioerr(format!("rejected: {}", String::from_utf8_lossy(ack.get(33..).unwrap_or(&[])))));
        }
        let mut h = [0u8; 32];
        h.copy_from_slice(&ack[..32]);
        Ok(h)
    }
    /// current-state -> the folded index bytes (decode with store_protocol::decode_state).
    fn current_state(&mut self) -> io::Result<Vec<u8>> {
        self.stream.write_all(&frame(QUERY, &[Q_STATE]))?;
        let payload = read_until(&mut self.stream, QUERY_REPLY)?;
        match payload.split_first() {
            Some((k, rest)) if *k == Q_STATE => Ok(rest.to_vec()),
            _ => Err(ioerr("bad query reply".into())),
        }
    }
}

/// Connect to the FIRST live index endpoint in a comma-separated list -- automatic
/// publisher/reader failover across HA peers, no leader election (the LWW register + a
/// multi-writer allow-list make writing via any live peer safe).
fn connect_index(index_flag: &str) -> Index {
    let addrs: Vec<&str> = index_flag.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    for a in &addrs {
        match Index::connect(a) {
            Ok(idx) => return idx,
            Err(e) => eprintln!("store: index {a} unreachable ({e}); trying next peer..."),
        }
    }
    die(&format!("no live index endpoint among: {index_flag}"));
}

fn resolve(index_flag: &str, name: &str) -> Result<String, String> {
    let mut idx = connect_index(index_flag);
    let bytes = idx.current_state().map_err(|e| format!("current-state: {e}"))?;
    let st = decode_state(&bytes).ok_or("decode index state")?;
    st.entries
        .iter()
        .find(|e| e.name == name && !e.tombstone)
        .map(|e| e.hash.clone())
        .ok_or_else(|| format!("name not in index: {name}"))
}

// ============================ content wire ============================
// Peer<->peer over tcp: [op:u8][hash:32 raw][len:u32 BE][payload]. Matches content-node.
const OP_REQ_GET: u8 = 1;
const OP_BLOB: u8 = 2;
const OP_PUSH: u8 = 4;

fn content_frame(op: u8, hash: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut f = vec![op];
    f.extend_from_slice(hash);
    f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    f.extend_from_slice(payload);
    f
}
fn read_content_frame(s: &mut TcpStream) -> io::Result<(u8, [u8; 32], Vec<u8>)> {
    let mut hdr = [0u8; 37];
    s.read_exact(&mut hdr)?;
    let op = hdr[0];
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&hdr[1..33]);
    let len = u32::from_be_bytes([hdr[33], hdr[34], hdr[35], hdr[36]]) as usize;
    let mut payload = vec![0u8; len];
    s.read_exact(&mut payload)?;
    Ok((op, hash, payload))
}
/// PUSH bytes to a holder (replication / publish); best-effort ACK read.
fn content_push(addr: &str, hash: &[u8], bytes: &[u8]) -> Result<(), String> {
    let mut s = TcpStream::connect(addr).map_err(|e| format!("connect holder {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(8))).ok();
    s.write_all(&content_frame(OP_PUSH, hash, bytes)).map_err(|e| format!("push: {e}"))?;
    let _ = read_content_frame(&mut s); // ACK (best-effort)
    Ok(())
}
/// Fetch bytes for `hash` from a holder, verifying the SHA-256 on receipt.
fn content_fetch(addr: &str, hash: &[u8]) -> Result<Vec<u8>, String> {
    let mut s = TcpStream::connect(addr).map_err(|e| format!("connect holder {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(8))).ok();
    s.write_all(&content_frame(OP_REQ_GET, hash, &[])).map_err(|e| format!("req_get: {e}"))?;
    let (op, rh, payload) = read_content_frame(&mut s).map_err(|e| format!("read blob: {e}"))?;
    if op != OP_BLOB {
        return Err(format!("holder MISS for {}", hex(hash)));
    }
    if &rh[..] != hash || sha256(&payload) != hash {
        return Err("integrity: fetched bytes do not match the requested hash".into());
    }
    Ok(payload)
}

// ============================ helpers ============================
fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}
fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}
fn ioerr(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::Other, m)
}
/// Content-addressed package path (supervisor-dev's convention): <root>/packages/<sha256>.wasm.
/// Immutable + dedups; a refresh writes a NEW digest path (never clobbers a live actor's file).
fn package_path(root: &str, hash_hex: &str) -> String {
    format!("{root}/packages/{hash_hex}.wasm")
}

// ============================ arg parsing ============================
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}
fn need(args: &[String], name: &str) -> String {
    flag(args, name).unwrap_or_else(|| die(&format!("missing required {name}")))
}
fn die(msg: &str) -> ! {
    eprintln!("store: {msg}");
    process::exit(1);
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_default();
    let rest = &args[args.len().min(1)..];
    match cmd.as_str() {
        "init" => cmd_init(rest),
        "publish" => cmd_publish(rest),
        "materialize" => cmd_materialize(rest),
        "resolve" => cmd_resolve(rest),
        "gc" => cmd_gc(rest),
        _ => die("usage: store <init|publish|materialize|resolve|gc> ...  (see the source header)"),
    }
}

/// Author the genesis event: allow-list the writer node(s). `--node-seed S` = single-writer
/// (allow-list [pubkey(S)]); `--allow s1,s2,s3` = multi-writer (write-HA -- any listed peer may
/// author; the LWW register converges concurrent/failover writes with no consensus).
fn cmd_init(a: &[String]) {
    let index = need(a, "--index");
    let allow: Vec<Vec<u8>> = match flag(a, "--allow") {
        Some(seeds) => seeds.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).map(node_pubkey).collect(),
        None => vec![node_pubkey(&need(a, "--node-seed"))],
    };
    if allow.is_empty() {
        die("init needs --node-seed <S> or --allow <s1,s2,...>");
    }
    let mut idx = connect_index(&index);
    idx.submit(&encode(&Cmd::Genesis { allow_list: allow.clone() })).unwrap_or_else(|e| die(&format!("genesis: {e}")));
    let short: Vec<String> = allow.iter().map(|pk| pk.iter().take(4).map(|b| format!("{:02x}", b)).collect()).collect();
    println!("initialized index: allow-listed {} writer node(s): {}", allow.len(), short.join(", "));
}

/// Central authorized publish: PUSH the wasm to a content holder + author name->hash on the index.
fn cmd_publish(a: &[String]) {
    let name = need(a, "--name");
    let wasm = need(a, "--wasm");
    let holder = need(a, "--holder");
    let index = need(a, "--index");
    let bytes = fs::read(&wasm).unwrap_or_else(|e| die(&format!("read {wasm}: {e}")));
    let h = sha256(&bytes);
    let hh = hex(&h);
    // Replicate to EVERY holder in the roster (RF = #holders) -- content durability.
    let holders: Vec<&str> = holder.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let mut replicated = 0;
    for hd in &holders {
        match content_push(hd, &h, &bytes) {
            Ok(()) => replicated += 1,
            Err(e) => eprintln!("store: push to holder {hd} failed ({e})"),
        }
    }
    if replicated == 0 {
        die("published to NO holder (all pushes failed)");
    }
    let mut idx = connect_index(&index);
    idx.submit(&encode(&Cmd::Put { name: name.clone(), hash: hh.clone() })).unwrap_or_else(|e| die(&format!("put: {e}")));
    println!("published {} ({} bytes) -> {}  (replicated to {}/{} holders, index {})", name, bytes.len(), hh, replicated, holders.len(), index);
}

/// Materialize a name to a content-addressed local path: resolve via index, local-cache or
/// fetch-by-hash + verify, write to <root>/packages/<hash>.wasm (idempotent; boot-local after).
fn cmd_materialize(a: &[String]) {
    let name = need(a, "--name");
    let index = need(a, "--index");
    let holder = need(a, "--holder");
    let root = flag(a, "--root").unwrap_or_else(|| ".".into());
    let hh = resolve(&index, &name).unwrap_or_else(|e| die(&format!("resolve: {e}")));
    let hash = unhex(&hh).unwrap_or_else(|| die("index hash is not 32-byte hex"));
    let out = flag(a, "--out").unwrap_or_else(|| package_path(&root, &hh));
    // idempotent + boot-local: if the content-addressed file already exists + verifies, done.
    if let Ok(existing) = fs::read(&out) {
        if sha256(&existing) == hash {
            println!("{out}  (cached, verified)");
            return;
        }
    }
    // Fetch from the FIRST live holder that serves the hash (read failover across replicas).
    let holders: Vec<&str> = holder.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let bytes = holders
        .iter()
        .find_map(|hd| content_fetch(hd, &hash).ok())
        .unwrap_or_else(|| die(&format!("fetch {hh}: no holder among {} served it", holders.len())));
    if let Some(parent) = std::path::Path::new(&out).parent() {
        fs::create_dir_all(parent).ok();
    }
    // write to a temp then rename -- never clobber a file a live actor may be reading.
    let tmp = format!("{out}.tmp.{}", process::id());
    fs::write(&tmp, &bytes).unwrap_or_else(|e| die(&format!("write {tmp}: {e}")));
    fs::rename(&tmp, &out).unwrap_or_else(|e| die(&format!("rename -> {out}: {e}")));
    println!("{out}  ({} bytes, sha256 {} verified)", bytes.len(), hh);
}

fn cmd_resolve(a: &[String]) {
    let name = need(a, "--name");
    let index = need(a, "--index");
    println!("{}", resolve(&index, &name).unwrap_or_else(|e| die(&e)));
}

/// GC-by-liveness (nix-gc-roots): drop on-box CAS files whose hash is referenced by NO live
/// index entry. The index is the GC root -- content stays live while some name points at it.
/// Safe: an immutable blob dropped here can be re-materialized if a future index entry needs it.
fn cmd_gc(a: &[String]) {
    let index = need(a, "--index");
    let root = flag(a, "--root").unwrap_or_else(|| ".".into());
    let mut idx = connect_index(&index);
    let bytes = idx.current_state().unwrap_or_else(|e| die(&format!("current-state: {e}")));
    let st = decode_state(&bytes).unwrap_or_else(|| die("decode index state"));
    let live: Vec<String> = st.entries.iter().filter(|e| !e.tombstone).map(|e| e.hash.clone()).collect();
    let dir = format!("{root}/packages");
    let rd = fs::read_dir(&dir).unwrap_or_else(|e| die(&format!("read {dir}: {e}")));
    let (mut kept, mut dropped) = (0u32, 0u32);
    for ent in rd.flatten() {
        let path = ent.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let hash = fname.strip_suffix(".wasm").unwrap_or(&fname);
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue; // not a CAS object
        }
        if live.iter().any(|l| l == hash) {
            kept += 1;
        } else {
            match fs::remove_file(&path) {
                Ok(()) => {
                    dropped += 1;
                    println!("gc: dropped {hash}");
                }
                Err(e) => eprintln!("gc: remove {fname}: {e}"),
            }
        }
    }
    println!("gc: kept {kept}, dropped {dropped}  (live index roots = {})", live.len());
}
