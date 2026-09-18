# store-tunnel — static TLS tunnel for the store transport (b')

The store's mesh (index :9700) + content (holder :9710) ports stay **private** on the VPS cluster;
this tiny static-musl proxy carries the WAN hop between inbox-dev's container and the cluster over
TLS. It adds **confidentiality only** — the mesh already signs every event (ed25519) and authenticates
peers in the handshake, so integrity + authenticity hold on any channel (mesh-dev, id=75). This is the
Colin-chosen (b') encrypted-tunnel transport (id=77): no wireguard, no kernel caps, no container recreate.

One statically-linked binary (rustls + ring, cert-pinned), three modes:

```
store-tunnel gen-cert --out DIR [--name NAME]
    Emit a self-signed cert.pem + key.pem (SAN dns = NAME, default "store-proxy").

store-tunnel server --cert C --key K --route L=T [--route L2=T2 ...]
    TLS-listen L, forward the decrypted stream to plaintext T (the cluster loopback port).

store-tunnel client --ca cert.pem --name NAME --route L=T [--route ...]
    Plaintext-listen L (the node/CLI dials here), TLS-dial T (the proxy). PINS the server cert
    (exact DER match) — only OUR cert is trusted, no CA/PKI, no name trust beyond the pin.
```

## Deploy shape (per the manager, id=77)
```sh
# once: mint the pinned keypair (keep key.pem on the VPS; ship cert.pem to the client box)
store-tunnel gen-cert --out /etc/store/tls --name store-proxy

# VPS (manager deploys): front the PRIVATE cluster ports; open only 19700/19710 to the WAN.
store-tunnel server --cert /etc/store/tls/cert.pem --key /etc/store/tls/key.pem \
  --route 0.0.0.0:19700=127.0.0.1:9700 \
  --route 0.0.0.0:19710=127.0.0.1:9710

# inbox-dev's RUNNING container (curl the binary in, like the supervisor binary):
store-tunnel client --ca /etc/store/tls/cert.pem --name store-proxy \
  --route 127.0.0.1:9700=<VPS_PUBLIC>:19700 \
  --route 127.0.0.1:9710=<VPS_PUBLIC>:19710
```
The writer's node then dials **127.0.0.1:9700** (index) and the CLI publishes to **127.0.0.1:9710**
(holder) — the local tunnel endpoints. mesh/node config is otherwise unchanged; only the dial address
is the local endpoint instead of a public IP (mesh-dev). The cluster mesh ports never touch the public
internet. For a full-mesh dial to N peers, give the client one `--route 127.0.0.1:970x=<PEERx>:19700`
per peer and point the node's dial list at those local ports (pubkeys unchanged).

## Security model
- **Confidentiality:** TLS 1.2/1.3 (ring). A WAN eavesdropper sees ciphertext.
- **Pinning:** the client trusts EXACTLY the cert in `--ca` (DER equality in a custom verifier) — a
  wrong/rotated cert is rejected with `pinned-cert mismatch`. No CA infrastructure to run. Rotating
  the cert = re-`gen-cert` + redistribute cert.pem to clients.
- **Defense-in-depth:** this layers under the mesh's existing signing/handshake auth; it is not the
  integrity mechanism, only the confidentiality wrapper.

## Proven (local)
- Full store protocol (init + publish + resolve) end-to-end through the pinned tunnel
  (CLI → client → TLS → server → node) → resolve returned the exact published content hash.
- Negative: a client pinning a DIFFERENT self-signed cert → `pinned-cert mismatch`, connection refused.
- Static binary: `gen-cert` (ring init) + TLS handshake + pinning all verified on the static-musl build.

## Build
`store-tunnel` here is the static-musl binary (statically linked, ~2.4 MB, zero deps — scp/curl-able).
Rebuild: `nix build .#default` (nixos-25.05 pkgsStatic; ring needs `perl` at build time, already in the
flake's nativeBuildInputs). `time` is pinned to 0.3.36 for the pinned rustc (1.86).
