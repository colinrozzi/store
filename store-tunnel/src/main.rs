//! store-tunnel -- a tiny static-musl TLS tunnel for the store transport (b').
//!
//! The store's mesh + content ports stay PRIVATE on the VPS cluster; this proxy carries the
//! WAN hop between inbox-dev's container and the cluster over TLS. Integrity/auth are already
//! covered by the mesh (ed25519-signed events + authenticated handshake); this layer adds
//! CONFIDENTIALITY only -- defense in depth over the public internet.
//!
//! One binary, three modes:
//!   gen-cert --out DIR [--name NAME]     -- emit a self-signed cert.pem + key.pem (SAN = NAME).
//!   server --cert C --key K --route L=T [--route ...]   -- TLS-listen L, forward plaintext to T
//!                                                          (T = cluster loopback, e.g. 127.0.0.1:9700).
//!   client --ca cert.pem --name NAME --route L=T [--route ...] -- plaintext-listen L (the node dials
//!                                                          here), TLS-dial T (the proxy). Pins the
//!                                                          server cert (exact DER match), so only OUR
//!                                                          cert is trusted -- no CA/PKI.
//!
//! Deploy shape (per the manager, id=77):
//!   VPS:  store-tunnel server --cert c --key k \
//!           --route 0.0.0.0:19700=127.0.0.1:9700  --route 0.0.0.0:19710=127.0.0.1:9710
//!   box:  store-tunnel client --ca c --name store-proxy \
//!           --route 127.0.0.1:9700=<VPS>:19700    --route 127.0.0.1:9710=<VPS>:19710
//!   node dials 127.0.0.1:9700 / :9710 (local tunnel) -> encrypted to the private cluster ports.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

type Err = Box<dyn std::error::Error + Send + Sync>;

fn die(msg: &str) -> ! {
    eprintln!("store-tunnel: {msg}");
    std::process::exit(1);
}

/// A --route L=T pair.
#[derive(Clone)]
struct Route {
    listen: String,
    target: String,
}

fn args() -> Vec<String> {
    std::env::args().skip(1).collect()
}
fn flag(a: &[String], name: &str) -> Option<String> {
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1)).cloned()
}
fn need(a: &[String], name: &str) -> String {
    flag(a, name).unwrap_or_else(|| die(&format!("missing {name}")))
}
fn routes(a: &[String]) -> Vec<Route> {
    let rs: Vec<Route> = a
        .iter()
        .enumerate()
        .filter(|(_, x)| x.as_str() == "--route")
        .filter_map(|(i, _)| a.get(i + 1))
        .map(|spec| {
            let (l, t) = spec.split_once('=').unwrap_or_else(|| die("--route must be LISTEN=TARGET"));
            Route { listen: l.to_string(), target: t.to_string() }
        })
        .collect();
    if rs.is_empty() {
        die("need at least one --route LISTEN=TARGET");
    }
    rs
}

fn main() {
    let a = args();
    let mode = a.first().cloned().unwrap_or_default();
    let rest = &a[a.len().min(1)..];
    match mode.as_str() {
        "gen-cert" => gen_cert(rest),
        "server" => run(rest, false),
        "client" => run(rest, true),
        _ => die("usage: store-tunnel <gen-cert|server|client> ...  (see the source header)"),
    }
}

fn gen_cert(a: &[String]) {
    let out = need(a, "--out");
    let name = flag(a, "--name").unwrap_or_else(|| "store-proxy".into());
    let cert = rcgen::generate_simple_self_signed(vec![name.clone()])
        .unwrap_or_else(|e| die(&format!("gen cert: {e}")));
    std::fs::create_dir_all(&out).ok();
    std::fs::write(format!("{out}/cert.pem"), cert.cert.pem())
        .unwrap_or_else(|e| die(&format!("write cert: {e}")));
    std::fs::write(format!("{out}/key.pem"), cert.key_pair.serialize_pem())
        .unwrap_or_else(|e| die(&format!("write key: {e}")));
    println!("wrote {out}/cert.pem + {out}/key.pem  (SAN dns={name})");
    println!("  server: store-tunnel server --cert {out}/cert.pem --key {out}/key.pem --route ...");
    println!("  client: store-tunnel client --ca {out}/cert.pem --name {name} --route ...  (pins THIS cert)");
}

fn load_certs(path: &str) -> Vec<CertificateDer<'static>> {
    let data = std::fs::read(path).unwrap_or_else(|e| die(&format!("read {path}: {e}")));
    rustls_pemfile::certs(&mut &data[..])
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| die(&format!("parse certs {path}: {e}")))
}
fn load_key(path: &str) -> PrivateKeyDer<'static> {
    let data = std::fs::read(path).unwrap_or_else(|e| die(&format!("read {path}: {e}")));
    rustls_pemfile::private_key(&mut &data[..])
        .unwrap_or_else(|e| die(&format!("parse key {path}: {e}")))
        .unwrap_or_else(|| die(&format!("no private key in {path}")))
}

/// A verifier that PINS one exact certificate (DER equality). Only our self-signed cert is
/// accepted -- no CA trust, no name checks beyond what pinning implies. Signature verification
/// delegates to the ring provider's algorithms (real TLS crypto, just a pinned trust anchor).
#[derive(Debug)]
struct Pinned {
    cert: CertificateDer<'static>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.cert.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("pinned-cert mismatch".into()))
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn run(a: &[String], client: bool) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| die(&format!("runtime: {e}")));
    rt.block_on(async move {
        let rs = routes(a);
        if client {
            let ca = need(a, "--ca");
            let name = flag(a, "--name").unwrap_or_else(|| "store-proxy".into());
            let pin = load_certs(&ca).into_iter().next().unwrap_or_else(|| die("no cert in --ca"));
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let cfg = ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .unwrap_or_else(|e| die(&format!("tls versions: {e}")))
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Pinned { cert: pin, provider }))
                .with_no_client_auth();
            let connector = TlsConnector::from(Arc::new(cfg));
            let mut tasks = Vec::new();
            for r in rs {
                let connector = connector.clone();
                let name = name.clone();
                tasks.push(tokio::spawn(async move {
                    if let Err(e) = client_route(r, connector, name).await {
                        eprintln!("store-tunnel client route error: {e}");
                    }
                }));
            }
            for t in tasks {
                let _ = t.await;
            }
        } else {
            let cert = need(a, "--cert");
            let key = need(a, "--key");
            let certs = load_certs(&cert);
            let key = load_key(&key);
            let cfg = ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap_or_else(|e| die(&format!("server cert: {e}")));
            let acceptor = TlsAcceptor::from(Arc::new(cfg));
            let mut tasks = Vec::new();
            for r in rs {
                let acceptor = acceptor.clone();
                tasks.push(tokio::spawn(async move {
                    if let Err(e) = server_route(r, acceptor).await {
                        eprintln!("store-tunnel server route error: {e}");
                    }
                }));
            }
            for t in tasks {
                let _ = t.await;
            }
        }
    });
}

async fn server_route(r: Route, acceptor: TlsAcceptor) -> Result<(), Err> {
    let listener = TcpListener::bind(&r.listen).await?;
    eprintln!("[server] TLS {} -> {}", r.listen, r.target);
    loop {
        let (sock, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let target = r.target.clone();
        tokio::spawn(async move {
            let mut tls = match acceptor.accept(sock).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[server] tls handshake from {peer} failed: {e}");
                    return;
                }
            };
            match TcpStream::connect(&target).await {
                Ok(mut up) => {
                    let _ = copy_bidirectional(&mut tls, &mut up).await;
                }
                Err(e) => eprintln!("[server] connect {target}: {e}"),
            }
        });
    }
}

async fn client_route(r: Route, connector: TlsConnector, name: String) -> Result<(), Err> {
    let listener = TcpListener::bind(&r.listen).await?;
    eprintln!("[client] {} -> TLS {} (pinned, sni={name})", r.listen, r.target);
    let sni = ServerName::try_from(name).map_err(|_| "bad --name for SNI")?;
    loop {
        let (mut sock, _peer) = listener.accept().await?;
        let connector = connector.clone();
        let target = r.target.clone();
        let sni = sni.clone();
        tokio::spawn(async move {
            let up = match TcpStream::connect(&target).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[client] connect {target}: {e}");
                    return;
                }
            };
            match connector.connect(sni, up).await {
                Ok(mut tls) => {
                    let _ = copy_bidirectional(&mut sock, &mut tls).await;
                }
                Err(e) => eprintln!("[client] tls to {target}: {e}"),
            }
        });
    }
}
