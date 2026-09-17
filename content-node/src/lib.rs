//! # content-node — the store's content-transport actor (D2 v0a)
//!
//! Fetch-by-hash for the immutable content layer (see ../CONTENT-TRANSPORT-DESIGN.md): a
//! box missing a hash fetches the bytes from a peer that holds them, over its OWN tcp
//! (NOT the mesh node's DAG transport), and **verifies the SHA-256 on receipt**. The local
//! byte sink is theater's content store (same CAS `content-store/` wraps).
//!
//! ## v0a — self-orchestrating proof (no external driver)
//! Two content-nodes over tcp:
//! - a **server** (`role=server;listen=ADDR;seed=TEXT`) puts `TEXT` into its store and
//!   serves `REQ_GET` requests with the bytes;
//! - a **client** (`role=client;peer=ADDR;expect=SHA256HEX`) connects on init, sends
//!   `REQ_GET expect`, and on the `BLOB` reply re-verifies the SHA-256 and shuts down with
//!   `content-node-fetch-passed` (or `-failed:<reason>`). This mirrors the `content-store`
//!   self-test, but the bytes cross a real tcp hop between two actors.
//!
//! ## Wire framing (self-framed, one connection carries many frames)
//! `[op:u8][hash:32 raw sha256][len:u32 BE][payload:len]`
//!   op 1 REQ_GET (payload empty) · op 2 BLOB (payload = bytes) · op 3 MISS (payload empty)

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use packr_guest::{export, import, pack_types, GraphValue, Value, ValueType};
use sha2::{Digest, Sha256};
use theater_guest::State;

#[cfg(not(test))]
packr_guest::setup_guest!();

const OP_REQ_GET: u8 = 1;
const OP_BLOB: u8 = 2;
const OP_MISS: u8 = 3;
const HDR: usize = 1 + 32 + 4; // op + hash + len

/// A live connection's reassembly buffer (tcp is a stream; frames span `on-data` chunks).
#[derive(Clone, Default, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct Conn {
    pub id: String,
    pub buf: Vec<u8>,
}

/// The actor's persisted state (in-module `#[derive(State)]` cell).
#[derive(Clone, Default, GraphValue, State)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct NodeState {
    pub store_id: String,
    /// true = client (fetcher); false = server (serves REQ_GET from its store).
    pub client: bool,
    /// client only: the raw 32-byte SHA-256 it expects to fetch + verify.
    pub expect: Vec<u8>,
    pub conns: Vec<Conn>,
}

pack_types! {
    imports {
        theater:simple/self {
            log: func(msg: string),
            shutdown: func(data: option<list<u8>>) -> result<_, string>,
        }
        theater:simple/tcp {
            listen: func(address: string) -> result<string, string>,
            connect: func(address: string) -> result<string, string>,
            activate: func(connection-id: string) -> result<_, string>,
            set-active: func(connection-id: string, mode: string) -> result<_, string>,
            send: func(connection-id: string, data: list<u8>) -> result<u64, string>,
            close: func(connection-id: string) -> result<_, string>,
        }
        theater:simple/store {
            new: func() -> result<string, string>,
            store-at-label: func(store-id: string, label: string, content: list<u8>) -> result<string, string>,
            get: func(store-id: string, content-ref: string) -> result<list<u8>, string>,
            get-by-label: func(store-id: string, label: string) -> result<option<string>, string>,
        }
    }
    exports {
        theater:simple/actor.init: func(config: value) -> result<_, string>,
        theater:simple/actor.get-state: func() -> value,
        theater:simple/tcp-client.handle-connection: func(connection-id: string) -> result<_, string>,
        theater:simple/tcp-client.on-data: func(connection-id: string, data: list<u8>) -> result<_, string>,
        theater:simple/tcp-client.on-close: func(connection-id: string, reason: string) -> result<_, string>,
    }
}

// ---- host imports ----
#[import(module = "theater:simple/self", name = "log")]
fn log(msg: String);
#[import(module = "theater:simple/self", name = "shutdown")]
fn shutdown(data: Option<Vec<u8>>) -> Result<(), String>;
#[import(module = "theater:simple/tcp", name = "listen")]
fn tcp_listen(address: String) -> Result<String, String>;
#[import(module = "theater:simple/tcp", name = "connect")]
fn tcp_connect(address: String) -> Result<String, String>;
#[import(module = "theater:simple/tcp", name = "activate")]
fn tcp_activate(conn_id: String) -> Result<(), String>;
#[import(module = "theater:simple/tcp", name = "set-active")]
fn tcp_set_active(conn_id: String, mode: String) -> Result<(), String>;
#[import(module = "theater:simple/tcp", name = "send")]
fn tcp_send(conn_id: String, data: Vec<u8>) -> Result<u64, String>;
#[import(module = "theater:simple/tcp", name = "close")]
fn tcp_close(conn_id: String) -> Result<(), String>;
#[import(module = "theater:simple/store", name = "new")]
fn store_new() -> Result<String, String>;
#[import(module = "theater:simple/store", name = "store-at-label")]
fn store_at_label(store_id: String, label: String, content: Vec<u8>) -> Result<String, String>;
#[import(module = "theater:simple/store", name = "get")]
fn store_get_ref(store_id: String, content_ref: String) -> Result<Vec<u8>, String>;
#[import(module = "theater:simple/store", name = "get-by-label")]
fn store_get_by_label(store_id: String, label: String) -> Result<Option<String>, String>;

// ---- CAS helpers (SHA-256 owned; theater store is the opaque byte sink) ----
fn sha256(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}
fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0xf) as usize] as char);
    }
    s
}
fn unhex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let v = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks(2) {
        out.push((v(pair[0])? << 4) | v(pair[1])?);
    }
    Some(out)
}
/// Store `bytes`, returning the raw 32-byte SHA-256 (label is the hex form).
fn cas_put(store_id: &str, bytes: Vec<u8>) -> Vec<u8> {
    let digest = sha256(&bytes);
    let _ = store_at_label(store_id.to_string(), hex(&digest), bytes);
    digest
}
/// Fetch the bytes for a raw 32-byte hash from the local store (None if absent).
fn cas_get(store_id: &str, hash: &[u8]) -> Option<Vec<u8>> {
    let label = hex(hash);
    let theater_ref = store_get_by_label(store_id.to_string(), label).ok()??;
    store_get_ref(store_id.to_string(), theater_ref).ok()
}

// ---- wire framing ----
fn frame(op: u8, hash: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(HDR + payload.len());
    f.push(op);
    f.extend_from_slice(hash); // 32 bytes
    f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    f.extend_from_slice(payload);
    f
}
/// Drain complete frames from `buf`, returning `(op, hash, payload)` for each.
fn take_frames(buf: &mut Vec<u8>) -> Vec<(u8, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    loop {
        if buf.len() < HDR {
            break;
        }
        let op = buf[0];
        let hash = buf[1..33].to_vec();
        let len = u32::from_be_bytes([buf[33], buf[34], buf[35], buf[36]]) as usize;
        if buf.len() < HDR + len {
            break;
        }
        let payload = buf[HDR..HDR + len].to_vec();
        buf.drain(0..HDR + len);
        out.push((op, hash, payload));
    }
    out
}

// ---- config: a tiny `k=v;k=v` string (we own the manifest initial_state) ----
fn cfg_get<'a>(cfg: &'a str, key: &str) -> Option<&'a str> {
    cfg.split(';')
        .find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

// ---- theater lifecycle ----
#[export(name = "theater:simple/actor.init")]
fn init(config: Value) -> Value {
    let cfg = match config {
        Value::String(s) if !s.is_empty() => s,
        _ => return err("missing config (role=…;…)"),
    };
    let store_id = match store_new() {
        Ok(id) => id,
        Err(e) => return err(&format!("store new: {}", e)),
    };
    let role = cfg_get(&cfg, "role").unwrap_or("server");

    if role == "client" {
        let peer = match cfg_get(&cfg, "peer") {
            Some(p) => p.to_string(),
            None => return err("client needs peer=ADDR"),
        };
        let expect = match cfg_get(&cfg, "expect").and_then(unhex) {
            Some(h) if h.len() == 32 => h,
            _ => return err("client needs expect=<64-hex sha256>"),
        };
        // Dial the peer and request the hash. The BLOB reply arrives via on-data.
        let conn = match tcp_connect(peer.clone()) {
            Ok(c) => c,
            Err(e) => return err(&format!("connect {}: {}", peer, e)),
        };
        let _ = tcp_activate(conn.clone());
        let _ = tcp_set_active(conn.clone(), "active".to_string());
        let _ = tcp_send(conn.clone(), frame(OP_REQ_GET, &expect, &[]));
        log(format!("[client] REQ_GET {} -> {}", hex(&expect), peer));
        NodeState::set(NodeState {
            store_id,
            client: true,
            expect,
            conns: vec![Conn { id: conn, buf: Vec::new() }],
        });
    } else {
        let seed = cfg_get(&cfg, "seed").unwrap_or("").as_bytes().to_vec();
        let listen = cfg_get(&cfg, "listen").unwrap_or("127.0.0.1:0").to_string();
        let hash = cas_put(&store_id, seed);
        match tcp_listen(listen.clone()) {
            Ok(id) => log(format!("[server] listening {} (id={}), holds {}", listen, id, hex(&hash))),
            Err(e) => return err(&format!("listen {}: {}", listen, e)),
        }
        NodeState::set(NodeState { store_id, client: false, expect: Vec::new(), conns: Vec::new() });
    }
    ok_unit()
}

#[export(name = "theater:simple/tcp-client.handle-connection")]
fn handle_connection(conn_id: String) -> Value {
    // Inbound (server) connection: take ownership + stream mode, track its buffer.
    if tcp_activate(conn_id.clone()).is_err()
        || tcp_set_active(conn_id.clone(), "active".to_string()).is_err()
    {
        let _ = tcp_close(conn_id);
        return ok_unit();
    }
    NodeState::with_mut(|s| s.conns.push(Conn { id: conn_id, buf: Vec::new() }));
    ok_unit()
}

#[export(name = "theater:simple/tcp-client.on-data")]
fn on_data(conn_id: String, data: Vec<u8>) -> Value {
    // Reassemble frames for this connection, then act on each (host calls outside the cell).
    let (frames, store_id, client, expect) = NodeState::with_mut(|s| {
        let frames = match s.conns.iter_mut().find(|c| c.id == conn_id) {
            Some(c) => {
                c.buf.extend_from_slice(&data);
                take_frames(&mut c.buf)
            }
            None => Vec::new(),
        };
        (frames, s.store_id.clone(), s.client, s.expect.clone())
    });

    for (op, hash, payload) in frames {
        if !client && op == OP_REQ_GET {
            // server: answer with the bytes, or MISS
            match cas_get(&store_id, &hash) {
                Some(bytes) => {
                    let _ = tcp_send(conn_id.clone(), frame(OP_BLOB, &hash, &bytes));
                    log(format!("[server] served BLOB {} ({} bytes)", hex(&hash), bytes.len()));
                }
                None => {
                    let _ = tcp_send(conn_id.clone(), frame(OP_MISS, &hash, &[]));
                }
            }
        } else if client && op == OP_BLOB {
            // client: the whole point — re-verify the SHA-256 on receipt.
            let actual = sha256(&payload);
            if actual != hash {
                return finish(false, &format!("frame hash != content hash ({} vs {})", hex(&hash), hex(&actual)));
            }
            if hash != expect {
                return finish(false, &format!("got {} but expected {}", hex(&hash), hex(&expect)));
            }
            log(format!("[client] BLOB {} verified ({} bytes)", hex(&hash), payload.len()));
            return finish(true, "");
        } else if client && op == OP_MISS {
            return finish(false, "peer MISS: does not hold the hash");
        }
    }
    ok_unit()
}

#[export(name = "theater:simple/tcp-client.on-close")]
fn on_close(conn_id: String, _reason: String) -> Value {
    NodeState::with_mut(|s| s.conns.retain(|c| c.id != conn_id));
    ok_unit()
}

/// Client terminal: shut down with a pass/fail marker (the harness greps for it).
fn finish(ok: bool, reason: &str) -> Value {
    if ok {
        log("[client] === fetch-by-hash verified — PASS ===".to_string());
        let _ = shutdown(Some(b"content-node-fetch-passed".to_vec()));
    } else {
        log(format!("[client] FETCH FAILED: {}", reason));
        let _ = shutdown(Some(format!("content-node-fetch-failed: {}", reason).into_bytes()));
    }
    ok_unit()
}

// ---- result<_, string> helpers ----
fn ok_unit() -> Value {
    let unit = Value::Tuple(Vec::new());
    Value::Result {
        ok_type: unit.infer_type(),
        err_type: ValueType::String,
        value: Ok(alloc::boxed::Box::new(unit)),
    }
}
fn err(msg: &str) -> Value {
    let unit = Value::Tuple(Vec::new());
    Value::Result {
        ok_type: unit.infer_type(),
        err_type: ValueType::String,
        value: Err(alloc::boxed::Box::new(Value::String(msg.to_string()))),
    }
}
