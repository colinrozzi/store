//! store-wiki -- v1 of the fleet's collaborative WIKI, a store-publishd SIBLING (token-authed HTTPS)
//! over the SAME store cluster. The store's FIRST real application consumer (past infra): agents + Colin
//! read/write a shared, versioned knowledge base over HTTP.
//!
//! DESIGN (v1, escape-(a) confluent-tolerable -- no consensus/CRDT; see mesh DESIGN-sm-contract §5b):
//!   - a page's CURRENT version = a name->hash entry in the store index: `wiki/<page>` -> version-hash
//!     (the index SM's existing LWW register -- ZERO SM change).
//!   - a VERSION = a content-addressed CAS blob, git-style, carrying its parent:
//!       [parent: 32 bytes (zero=root)] [ts: 8 BE] [author_len: 2 BE] [author utf8] [content]
//!     so history + immutable old versions fall out of content-addressing; history WALKS parent links.
//!   - CONCURRENCY: optimistic, best-effort. PUT carries `If-Match: <parent_hash>` (or `none` to create).
//!     If head moved (a causally-prior edit landed) -> 409 + current content (rebase). Truly-CONCURRENT
//!     same-parent edits both pass -> the index LWW picks a winner by (ts,author,id); the loser branch is
//!     orphaned but still in CAS (recoverable by hash). A hard "one successor" invariant is NOT enforceable
//!     in-fold (§5b) -- v2's sequence-CRDT SM is where real-time merge lives; v1 is agents-over-HTTP.
//!
//!   store-wiki --listen 0.0.0.0:8444 --cert C --key K --token-file T --index <peer>:9700 --holder <h>:9710
//!
//! Routes (all but /health require `Authorization: Bearer <token>`; author attribution via `X-Wiki-Author`):
//!   GET    /wiki/<page>            -> 200 body=content, header `ETag: <head_hash>` | 404
//!   PUT    /wiki/<page>            If-Match: <parent_hash|none>, body=content
//!                                  -> 200 {"head_hash":"..."} + ETag | 409 body=current content + ETag
//!   GET    /wiki/<page>/history    -> 200 [{"hash","ts","author"}...] (newest first) | 404
//!   GET    /wiki/<page>@<hash>     -> 200 body=that exact immutable version | 404
//!   GET    /health                -> 200 ok (no auth)

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use store_protocol::{decode_state, encode, Cmd};

fn die(m: &str) -> ! {
    eprintln!("store-wiki: {m}");
    std::process::exit(1);
}
fn args() -> Vec<String> {
    std::env::args().skip(1).collect()
}
fn flag(a: &[String], n: &str) -> Option<String> {
    a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned()
}
fn need(a: &[String], n: &str) -> String {
    flag(a, n).unwrap_or_else(|| die(&format!("missing {n}")))
}
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ===================== mesh client (inlined, from store-cli/publishd) =====================
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
fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() + 1) as u32;
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    out
}
fn ioerr(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::Other, m)
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
        let key = seeded_key("store-cli");
        s.write_all(&frame(HELLO, &key.verifying_key().to_bytes()))?;
        let nonce = read_until(&mut s, CHALLENGE)?;
        s.write_all(&frame(AUTH, &key.sign(&nonce).to_bytes()))?;
        read_until(&mut s, ACCEPTED)?;
        Ok(Index { stream: s })
    }
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
    fn current_state(&mut self) -> io::Result<Vec<u8>> {
        self.stream.write_all(&frame(QUERY, &[Q_STATE]))?;
        let payload = read_until(&mut self.stream, QUERY_REPLY)?;
        match payload.split_first() {
            Some((k, rest)) if *k == Q_STATE => Ok(rest.to_vec()),
            _ => Err(ioerr("bad query reply".into())),
        }
    }
}

// ===================== content wire (inlined; push + fetch) =====================
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
fn content_push(addr: &str, hash: &[u8], bytes: &[u8]) -> Result<(), String> {
    let mut s = TcpStream::connect(addr).map_err(|e| format!("connect holder {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(8))).ok();
    s.write_all(&content_frame(OP_PUSH, hash, bytes)).map_err(|e| format!("push: {e}"))?;
    let mut hdr = [0u8; 37];
    let _ = s.read_exact(&mut hdr); // ACK best-effort
    Ok(())
}
/// Fetch a blob by hash from the FIRST holder that serves it, SHA-256-verified on receipt.
fn content_fetch_any(holders: &[String], hash: &[u8; 32]) -> Result<Vec<u8>, String> {
    for addr in holders {
        if let Ok(bytes) = content_fetch(addr, hash) {
            return Ok(bytes);
        }
    }
    Err(format!("no holder served {} among {}", hex(hash), holders.len()))
}
fn content_fetch(addr: &str, hash: &[u8; 32]) -> Result<Vec<u8>, String> {
    let mut s = TcpStream::connect(addr).map_err(|e| format!("connect holder {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(8))).ok();
    s.write_all(&content_frame(OP_REQ_GET, hash, &[])).map_err(|e| format!("req_get: {e}"))?;
    let (op, rh, payload) = read_content_frame(&mut s).map_err(|e| format!("read blob: {e}"))?;
    if op != OP_BLOB || &rh != hash || &sha256(&payload) != hash {
        return Err(format!("holder MISS/mismatch for {}", hex(hash)));
    }
    Ok(payload)
}

fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}
fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}
fn unhex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for i in 0..a.len() {
        d |= a[i] ^ b[i];
    }
    d == 0
}

// ===================== version blob (git-style: parent + ts + author + content) =====================
fn build_version(parent: &[u8; 32], ts: u64, author: &str, content: &[u8]) -> Vec<u8> {
    let a = author.as_bytes();
    let mut v = Vec::with_capacity(32 + 8 + 2 + a.len() + content.len());
    v.extend_from_slice(parent);
    v.extend_from_slice(&ts.to_be_bytes());
    v.extend_from_slice(&(a.len() as u16).to_be_bytes());
    v.extend_from_slice(a);
    v.extend_from_slice(content);
    v
}
/// -> (parent, ts, author, content). None if not a well-formed version blob.
fn parse_version(blob: &[u8]) -> Option<([u8; 32], u64, String, Vec<u8>)> {
    if blob.len() < 42 {
        return None;
    }
    let mut parent = [0u8; 32];
    parent.copy_from_slice(&blob[..32]);
    let ts = u64::from_be_bytes(blob[32..40].try_into().ok()?);
    let alen = u16::from_be_bytes(blob[40..42].try_into().ok()?) as usize;
    if blob.len() < 42 + alen {
        return None;
    }
    let author = String::from_utf8_lossy(&blob[42..42 + alen]).to_string();
    let content = blob[42 + alen..].to_vec();
    Some((parent, ts, author, content))
}

// ===================== the blocking wiki work =====================
struct Cfg {
    index: String,
    holders: Vec<String>,
    token: Vec<u8>,
    max_body: usize,
}

fn resolve_head(cfg: &Cfg, key: &str) -> Result<Option<String>, String> {
    let mut idx = Index::connect(&cfg.index).map_err(|e| format!("index connect: {e}"))?;
    let bytes = idx.current_state().map_err(|e| format!("current-state: {e}"))?;
    let st = decode_state(&bytes).ok_or("decode index state")?;
    Ok(st.entries.iter().find(|e| e.name == key && !e.tombstone).map(|e| e.hash.clone()))
}

/// GET page -> (content, head_hash). None if absent.
fn do_get(cfg: &Cfg, page: &str) -> Result<Option<(Vec<u8>, String)>, String> {
    let key = format!("wiki/{page}");
    let head = match resolve_head(cfg, &key)? {
        Some(h) => h,
        None => return Ok(None),
    };
    let hb = unhex(&head).ok_or("index head is not 32-byte hex")?;
    let blob = content_fetch_any(&cfg.holders, &hb)?;
    let (_, _, _, content) = parse_version(&blob).ok_or("corrupt version blob")?;
    Ok(Some((content, head)))
}

enum PutResult {
    Ok { head: String },
    Conflict { head: String, content: Vec<u8> },
}

/// PUT page with optimistic concurrency (If-Match == current head repr, or "none" to create).
fn do_put(cfg: &Cfg, page: &str, if_match: &str, author: &str, content: &[u8]) -> Result<PutResult, String> {
    let key = format!("wiki/{page}");
    let current = resolve_head(cfg, &key)?;
    let current_repr = current.as_deref().unwrap_or("none");
    if if_match != current_repr {
        // stale -> hand back the current content for a rebase
        let cur_content = match &current {
            Some(h) => {
                let hb = unhex(h).ok_or("bad head")?;
                let b = content_fetch_any(&cfg.holders, &hb)?;
                parse_version(&b).map(|(_, _, _, c)| c).unwrap_or_default()
            }
            None => Vec::new(),
        };
        return Ok(PutResult::Conflict { head: current_repr.to_string(), content: cur_content });
    }
    let parent = match &current {
        Some(h) => unhex(h).ok_or("bad head")?,
        None => [0u8; 32],
    };
    let blob = build_version(&parent, now_ms(), author, content);
    let vh = sha256(&blob);
    let vhex = hex(&vh);
    let mut pushed = 0;
    for holder in &cfg.holders {
        match content_push(holder, &vh, &blob) {
            Ok(()) => pushed += 1,
            Err(e) => eprintln!("[wiki] push to {holder}: {e}"),
        }
    }
    if pushed == 0 {
        return Err("no holder accepted the version blob".into());
    }
    let mut idx = Index::connect(&cfg.index).map_err(|e| format!("index connect: {e}"))?;
    idx.submit(&encode(&Cmd::Put { name: key, hash: vhex.clone() })).map_err(|e| format!("put: {e}"))?;
    Ok(PutResult::Ok { head: vhex })
}

/// Walk the parent chain from head -> [(hash, ts, author)] newest-first. None if page absent.
fn do_history(cfg: &Cfg, page: &str) -> Result<Option<Vec<(String, u64, String)>>, String> {
    let key = format!("wiki/{page}");
    let mut cur = match resolve_head(cfg, &key)? {
        Some(h) => unhex(&h).ok_or("bad head")?,
        None => return Ok(None),
    };
    let mut out = Vec::new();
    for _ in 0..100_000 {
        if cur == [0u8; 32] {
            break;
        }
        let blob = content_fetch_any(&cfg.holders, &cur)?;
        let (parent, ts, author, _) = parse_version(&blob).ok_or("corrupt version blob")?;
        out.push((hex(&cur), ts, author));
        cur = parent;
    }
    Ok(Some(out))
}

/// GET an exact version by hash -> content. None if absent / not a version blob.
fn do_get_version(cfg: &Cfg, hash_hex: &str) -> Result<Option<Vec<u8>>, String> {
    let hb = match unhex(hash_hex) {
        Some(h) => h,
        None => return Ok(None),
    };
    match content_fetch_any(&cfg.holders, &hb) {
        Ok(blob) => Ok(parse_version(&blob).map(|(_, _, _, c)| c)),
        Err(_) => Ok(None),
    }
}

// ===================== HTTP =====================
fn load_certs(p: &str) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    let d = std::fs::read(p).unwrap_or_else(|e| die(&format!("read {p}: {e}")));
    rustls_pemfile::certs(&mut &d[..]).collect::<Result<_, _>>().unwrap_or_else(|e| die(&format!("certs {p}: {e}")))
}
fn load_key(p: &str) -> rustls::pki_types::PrivateKeyDer<'static> {
    let d = std::fs::read(p).unwrap_or_else(|e| die(&format!("read {p}: {e}")));
    rustls_pemfile::private_key(&mut &d[..]).unwrap_or_else(|e| die(&format!("key {p}: {e}"))).unwrap_or_else(|| die("no key"))
}
fn json_escape(s: &str) -> String {
    s.chars().flat_map(|c| match c {
        '"' => vec!['\\', '"'],
        '\\' => vec!['\\', '\\'],
        '\n' => vec!['\\', 'n'],
        '\r' => vec!['\\', 'r'],
        '\t' => vec!['\\', 't'],
        c => vec![c],
    }).collect()
}

async fn respond<S: AsyncWriteExt + Unpin>(s: &mut S, code: u16, reason: &str, headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(body).await;
    let _ = s.flush().await;
}
async fn text<S: AsyncWriteExt + Unpin>(s: &mut S, code: u16, reason: &str, body: &str) {
    respond(s, code, reason, "Content-Type: text/plain\r\n", body.as_bytes()).await;
}

async fn handle<S: AsyncReadExt + AsyncWriteExt + Unpin>(mut s: S, cfg: Arc<Cfg>) {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 4096];
    let head_end;
    loop {
        match s.read(&mut tmp).await {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = find_head_end(&buf) {
                    head_end = p;
                    break;
                }
                if buf.len() > 16 * 1024 {
                    text(&mut s, 431, "Request Header Fields Too Large", "header too large\n").await;
                    return;
                }
            }
            Err(_) => return,
        }
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut auth = String::new();
    let mut content_length: usize = 0;
    let mut if_match = String::new();
    let mut author = "anonymous".to_string();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let kl = k.trim().to_ascii_lowercase();
            let v = v.trim();
            match kl.as_str() {
                "authorization" => auth = v.to_string(),
                "content-length" => content_length = v.parse().unwrap_or(0),
                "if-match" => if_match = v.trim_matches('"').to_string(),
                "x-wiki-author" => author = v.to_string(),
                _ => {}
            }
        }
    }

    if method == "GET" && path.starts_with("/health") {
        text(&mut s, 200, "OK", "ok\n").await;
        return;
    }

    let token_ok = auth.strip_prefix("Bearer ").map(|t| ct_eq(t.as_bytes(), &cfg.token)).unwrap_or(false);
    if !token_ok {
        text(&mut s, 401, "Unauthorized", "unauthorized\n").await;
        return;
    }

    let rest = match path.strip_prefix("/wiki/") {
        Some(r) => r.split('?').next().unwrap_or("").to_string(),
        None => {
            text(&mut s, 404, "Not Found", "no such route (expected /wiki/<page>)\n").await;
            return;
        }
    };

    // route within /wiki/: <page>/history | <page>@<hash> | <page>
    if let Some(page) = rest.strip_suffix("/history") {
        if method != "GET" {
            text(&mut s, 405, "Method Not Allowed", "history is GET\n").await;
            return;
        }
        let (cfg2, page2) = (cfg.clone(), page.to_string());
        match tokio::task::spawn_blocking(move || do_history(&cfg2, &page2)).await {
            Ok(Ok(Some(vs))) => {
                let items: Vec<String> = vs.iter().map(|(h, ts, a)| {
                    format!("{{\"hash\":\"{h}\",\"ts\":{ts},\"author\":\"{}\"}}", json_escape(a))
                }).collect();
                let body = format!("[{}]\n", items.join(","));
                respond(&mut s, 200, "OK", "Content-Type: application/json\r\n", body.as_bytes()).await;
            }
            Ok(Ok(None)) => text(&mut s, 404, "Not Found", "no such page\n").await,
            Ok(Err(e)) => text(&mut s, 502, "Bad Gateway", &format!("{e}\n")).await,
            Err(_) => text(&mut s, 500, "Internal Server Error", "join\n").await,
        }
        return;
    }
    if let Some((page, vhash)) = rest.rsplit_once('@') {
        if method != "GET" {
            text(&mut s, 405, "Method Not Allowed", "version fetch is GET\n").await;
            return;
        }
        let _ = page;
        let (cfg2, vh) = (cfg.clone(), vhash.to_string());
        match tokio::task::spawn_blocking(move || do_get_version(&cfg2, &vh)).await {
            Ok(Ok(Some(content))) => {
                respond(&mut s, 200, "OK", "Content-Type: application/octet-stream\r\n", &content).await;
            }
            Ok(Ok(None)) => text(&mut s, 404, "Not Found", "no such version\n").await,
            Ok(Err(e)) => text(&mut s, 502, "Bad Gateway", &format!("{e}\n")).await,
            Err(_) => text(&mut s, 500, "Internal Server Error", "join\n").await,
        }
        return;
    }
    let page = rest;
    if page.is_empty() {
        text(&mut s, 400, "Bad Request", "empty page name\n").await;
        return;
    }

    if method == "GET" {
        let (cfg2, page2) = (cfg.clone(), page.clone());
        match tokio::task::spawn_blocking(move || do_get(&cfg2, &page2)).await {
            Ok(Ok(Some((content, head)))) => {
                let hdr = format!("ETag: \"{head}\"\r\nContent-Type: application/octet-stream\r\n");
                respond(&mut s, 200, "OK", &hdr, &content).await;
            }
            Ok(Ok(None)) => text(&mut s, 404, "Not Found", "no such page\n").await,
            Ok(Err(e)) => text(&mut s, 502, "Bad Gateway", &format!("{e}\n")).await,
            Err(_) => text(&mut s, 500, "Internal Server Error", "join\n").await,
        }
        return;
    }

    if method == "PUT" {
        if content_length > cfg.max_body {
            text(&mut s, 413, "Payload Too Large", "content exceeds --max-body-mb\n").await;
            return;
        }
        if if_match.is_empty() {
            text(&mut s, 428, "Precondition Required", "PUT needs If-Match: <parent_hash|none>\n").await;
            return;
        }
        // drain body to exactly Content-Length
        let mut body = buf[head_end..].to_vec();
        if body.len() > content_length {
            body.truncate(content_length);
        }
        while body.len() < content_length {
            let mut chunk = vec![0u8; (content_length - body.len()).min(64 * 1024)];
            match s.read(&mut chunk).await {
                Ok(0) => {
                    text(&mut s, 400, "Bad Request", "short body\n").await;
                    return;
                }
                Ok(n) => body.extend_from_slice(&chunk[..n]),
                Err(_) => return,
            }
        }
        let (cfg2, page2, im, au) = (cfg.clone(), page.clone(), if_match.clone(), author.clone());
        match tokio::task::spawn_blocking(move || do_put(&cfg2, &page2, &im, &au, &body)).await {
            Ok(Ok(PutResult::Ok { head })) => {
                let hdr = format!("ETag: \"{head}\"\r\nContent-Type: application/json\r\n");
                respond(&mut s, 200, "OK", &hdr, format!("{{\"head_hash\":\"{head}\"}}\n").as_bytes()).await;
            }
            Ok(Ok(PutResult::Conflict { head, content })) => {
                let hdr = format!("ETag: \"{head}\"\r\nContent-Type: application/octet-stream\r\n");
                respond(&mut s, 409, "Conflict", &hdr, &content).await;
            }
            Ok(Err(e)) => text(&mut s, 502, "Bad Gateway", &format!("{e}\n")).await,
            Err(_) => text(&mut s, 500, "Internal Server Error", "join\n").await,
        }
        return;
    }

    text(&mut s, 405, "Method Not Allowed", "GET or PUT /wiki/<page>\n").await;
}

fn find_head_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn main() {
    let a = args();
    let listen = need(&a, "--listen");
    let cert = need(&a, "--cert");
    let key = need(&a, "--key");
    let token_file = need(&a, "--token-file");
    let index = need(&a, "--index");
    let holder = need(&a, "--holder");
    let max_body = flag(&a, "--max-body-mb").and_then(|s| s.parse::<usize>().ok()).unwrap_or(16) * 1024 * 1024;
    let token = std::fs::read(&token_file)
        .unwrap_or_else(|e| die(&format!("read token {token_file}: {e}")))
        .iter()
        .cloned()
        .filter(|b| !b"\r\n".contains(b))
        .collect::<Vec<u8>>();
    if token.is_empty() {
        die("token file is empty");
    }
    let holders: Vec<String> = holder.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

    let certs = load_certs(&cert);
    let pkey = load_key(&key);
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, pkey)
        .unwrap_or_else(|e| die(&format!("tls cert: {e}")));
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let cfg = Arc::new(Cfg { index, holders, token, max_body });

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap_or_else(|e| die(&format!("rt: {e}")));
    rt.block_on(async move {
        let l = TcpListener::bind(&listen).await.unwrap_or_else(|e| die(&format!("bind {listen}: {e}")));
        eprintln!("[wiki] https {listen} -> index {} holders {:?}", cfg.index, cfg.holders);
        loop {
            let (sock, _peer) = match l.accept().await {
                Ok(x) => x,
                Err(_) => continue,
            };
            let acceptor = acceptor.clone();
            let cfg = cfg.clone();
            tokio::spawn(async move {
                match acceptor.accept(sock).await {
                    Ok(tls) => handle(tls, cfg).await,
                    Err(e) => eprintln!("[wiki] tls handshake: {e}"),
                }
            });
        }
    });
}
