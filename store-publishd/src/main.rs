//! store-publishd -- the store's PERMANENT deploy interface (option B): a token-authed HTTPS
//! publish endpoint co-located with a cluster peer on the VPS. A deployer POSTs a wasm + a name +
//! a bearer token over plain outbound HTTPS (the fleet's curl+token pattern -- no mesh node, no
//! tunnel, no container change). The endpoint hashes + SHA-256-verified-pushes the bytes to the
//! local holder, submits Put(name->hash) to the co-located writer node (which authors+signs +
//! gossips), and RETURNS the stored hash so the caller verifies integrity against its local sha256.
//! The cluster mesh ports never leave the VPS; only this authed HTTPS front is public.
//!
//! Reuses the store CLI's proven mesh client + content wire (blocking std::net, run in
//! spawn_blocking off the async accept loop).
//!
//!   store-publishd --listen 0.0.0.0:8443 --cert C --key K --token-file T \
//!       --index 127.0.0.1:9700 --holder 127.0.0.1:9710 [--max-body-mb 64]
//!
//! Routes (all but /health require `Authorization: Bearer <token>`):
//!   POST /publish?name=<label>   body = raw wasm  -> 200 {"hash":"<64hex>","name":"<label>"}
//!   GET  /resolve?name=<label>                    -> 200 <64hex> | 404
//!   GET  /health                                  -> 200 ok   (no auth)

use std::sync::Arc;

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use store_protocol::{decode_state, encode, Cmd};

fn die(m: &str) -> ! {
    eprintln!("store-publishd: {m}");
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

// ===================== mesh client (inlined, from store-cli) =====================
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

// ===================== content wire (inlined, from store-cli) =====================
const OP_PUSH: u8 = 4;
fn content_frame(op: u8, hash: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut f = vec![op];
    f.extend_from_slice(hash);
    f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    f.extend_from_slice(payload);
    f
}
fn content_push(addr: &str, hash: &[u8], bytes: &[u8]) -> Result<(), String> {
    let mut s = TcpStream::connect(addr).map_err(|e| format!("connect holder {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(8))).ok();
    s.write_all(&content_frame(OP_PUSH, hash, bytes)).map_err(|e| format!("push: {e}"))?;
    // ACK (best-effort): read a small reply, ignore.
    let mut hdr = [0u8; 37];
    let _ = s.read_exact(&mut hdr);
    Ok(())
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
/// constant-time-ish equality for the bearer token
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

// ===================== the blocking publish/resolve work =====================
struct Cfg {
    index: String,
    holders: Vec<String>,
    token: Vec<u8>,
    max_body: usize,
}

/// PUSH content to all holders (RF; every push must succeed), then submit Put(name->hash).
fn do_publish(cfg: &Cfg, name: &str, wasm: &[u8]) -> Result<String, String> {
    let h = sha256(wasm);
    let hh = hex(&h);
    for holder in &cfg.holders {
        content_push(holder, &h, wasm)?;
    }
    let mut idx = Index::connect(&cfg.index).map_err(|e| format!("index connect: {e}"))?;
    idx.submit(&encode(&Cmd::Put { name: name.to_string(), hash: hh.clone() }))
        .map_err(|e| format!("put: {e}"))?;
    Ok(hh)
}
fn do_resolve(cfg: &Cfg, name: &str) -> Result<Option<String>, String> {
    let mut idx = Index::connect(&cfg.index).map_err(|e| format!("index connect: {e}"))?;
    let bytes = idx.current_state().map_err(|e| format!("current-state: {e}"))?;
    let st = decode_state(&bytes).ok_or("decode index state")?;
    Ok(st.entries.iter().find(|e| e.name == name && !e.tombstone).map(|e| e.hash.clone()))
}

// ===================== HTTP (minimal, drain-correct) =====================
fn load_certs(p: &str) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    let d = std::fs::read(p).unwrap_or_else(|e| die(&format!("read {p}: {e}")));
    rustls_pemfile::certs(&mut &d[..]).collect::<Result<_, _>>().unwrap_or_else(|e| die(&format!("certs {p}: {e}")))
}
fn load_key(p: &str) -> rustls::pki_types::PrivateKeyDer<'static> {
    let d = std::fs::read(p).unwrap_or_else(|e| die(&format!("read {p}: {e}")));
    rustls_pemfile::private_key(&mut &d[..]).unwrap_or_else(|e| die(&format!("key {p}: {e}"))).unwrap_or_else(|| die("no key"))
}

fn query_param(path: &str, key: &str) -> Option<String> {
    let q = path.split_once('?')?.1;
    for kv in q.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}

async fn respond<S: AsyncWriteExt + Unpin>(s: &mut S, code: u16, reason: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = s.write_all(msg.as_bytes()).await;
    let _ = s.flush().await;
}

async fn handle<S: AsyncReadExt + AsyncWriteExt + Unpin>(mut s: S, cfg: Arc<Cfg>) {
    // read head (request line + headers) up to \r\n\r\n, cap 16 KiB
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
                    respond(&mut s, 431, "Request Header Fields Too Large", "header too large\n").await;
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
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // headers
    let mut auth = String::new();
    let mut content_length: usize = 0;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "authorization" {
                auth = v.to_string();
            } else if k == "content-length" {
                content_length = v.parse().unwrap_or(0);
            }
        }
    }

    if method == "GET" && path.starts_with("/health") {
        respond(&mut s, 200, "OK", "ok\n").await;
        return;
    }

    // auth (all routes except /health)
    let token_ok = auth.strip_prefix("Bearer ").map(|t| ct_eq(t.as_bytes(), &cfg.token)).unwrap_or(false);
    if !token_ok {
        respond(&mut s, 401, "Unauthorized", "unauthorized\n").await;
        return;
    }

    if method == "GET" && path.starts_with("/resolve") {
        let name = match query_param(path, "name") {
            Some(n) => n,
            None => {
                respond(&mut s, 400, "Bad Request", "missing ?name=\n").await;
                return;
            }
        };
        let cfg2 = cfg.clone();
        let r = tokio::task::spawn_blocking(move || do_resolve(&cfg2, &name)).await;
        match r {
            Ok(Ok(Some(h))) => respond(&mut s, 200, "OK", &format!("{h}\n")).await,
            Ok(Ok(None)) => respond(&mut s, 404, "Not Found", "name not in index\n").await,
            Ok(Err(e)) => respond(&mut s, 502, "Bad Gateway", &format!("{e}\n")).await,
            Err(_) => respond(&mut s, 500, "Internal Server Error", "join\n").await,
        }
        return;
    }

    if method == "POST" && path.starts_with("/publish") {
        let name = match query_param(path, "name") {
            Some(n) => n,
            None => {
                respond(&mut s, 400, "Bad Request", "missing ?name=\n").await;
                return;
            }
        };
        if content_length == 0 {
            respond(&mut s, 400, "Bad Request", "empty body\n").await;
            return;
        }
        if content_length > cfg.max_body {
            respond(&mut s, 413, "Payload Too Large", "wasm exceeds --max-body-mb\n").await;
            return;
        }
        // body: whatever arrived past the head, then DRAIN the rest to exactly Content-Length.
        let mut body = buf[head_end..].to_vec();
        if body.len() > content_length {
            body.truncate(content_length);
        }
        while body.len() < content_length {
            let mut chunk = vec![0u8; (content_length - body.len()).min(64 * 1024)];
            match s.read(&mut chunk).await {
                Ok(0) => {
                    // short read = truncated upload; refuse (never author a corrupt deploy).
                    respond(&mut s, 400, "Bad Request", "short body (Content-Length not satisfied)\n").await;
                    return;
                }
                Ok(n) => body.extend_from_slice(&chunk[..n]),
                Err(_) => return,
            }
        }
        let cfg2 = cfg.clone();
        let name2 = name.clone();
        let r = tokio::task::spawn_blocking(move || do_publish(&cfg2, &name2, &body)).await;
        match r {
            Ok(Ok(h)) => respond(&mut s, 200, "OK", &format!("{{\"name\":\"{name}\",\"hash\":\"{h}\"}}\n")).await,
            Ok(Err(e)) => respond(&mut s, 502, "Bad Gateway", &format!("{e}\n")).await,
            Err(_) => respond(&mut s, 500, "Internal Server Error", "join\n").await,
        }
        return;
    }

    respond(&mut s, 404, "Not Found", "no such route\n").await;
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
    let max_body = flag(&a, "--max-body-mb").and_then(|s| s.parse::<usize>().ok()).unwrap_or(64) * 1024 * 1024;
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
        eprintln!("[publishd] https {listen} -> index {} holders {:?} (max-body {} MiB)", cfg.index, cfg.holders, cfg.max_body / 1024 / 1024);
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
                    Err(e) => eprintln!("[publishd] tls handshake: {e}"),
                }
            });
        }
    });
}
