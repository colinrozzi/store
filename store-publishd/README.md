# store-publishd — the store's permanent deploy interface (option B)

A token-authed **HTTPS publish endpoint**, co-located with a cluster peer on the VPS. A deployer pushes
a build with a plain outbound `curl -X POST` + a bearer token — the fleet's existing pattern (inbox API,
supervisor mgmt). No mesh node in the deployer's container, no TLS tunnel, no AddWriter, no gossip-timing
gate on the client side. The cluster mesh ports stay private on the VPS; only this authed HTTPS front is
public. (Chosen over an earlier own-key node+TLS-tunnel path — id=81/82.)

It reuses the store CLI's proven mesh client + content wire: on POST it SHA-256-hashes the wasm, pushes
the bytes to the local holder(s), and submits `Put(name -> hash)` to the co-located writer node (which
authors+signs + gossips). It RETURNS the stored hash so the caller compares it to its own `sha256(wasm)`
— that returned-hash check is the end-to-end integrity guarantee (it replaces the writer-node's
own-key authorship, since the write is now authored server-side by the co-located peer).

## Run (VPS, next to a cluster peer)
```sh
# cert: any TLS cert/key (self-signed is fine — callers pin it via --cacert). key.pem stays on the VPS.
mkdir -p /etc/store/tls && openssl req -x509 -newkey ed25519 -nodes -days 3650 \
  -subj "/CN=store-publishd" -addext "subjectAltName=DNS:<your-host>" \
  -keyout /etc/store/tls/key.pem -out /etc/store/tls/cert.pem      # ship cert.pem to callers
printf '%s' "$DEPLOY_TOKEN" > /etc/store/publish.token               # the bearer secret

store-publishd --listen 0.0.0.0:8443 \
  --cert /etc/store/tls/cert.pem --key /etc/store/tls/key.pem \
  --token-file /etc/store/publish.token \
  --index 127.0.0.1:9700 --holder 127.0.0.1:9710 \
  --materialize-root /var/lib/store [--max-body-mb 64]
#   --index  = the co-located writer peer's mesh listen port (it authors the Put + gossips)
#   --holder = local content holder(s), comma list for RF (bytes must land where the spine can fetch)
#   --materialize-root = REQUIRED when publishd is co-located with the consuming acceptor. On publish
#     it also writes <root>/packages/<hash>.wasm and atomically repoints <root>/by-name/<name>.wasm
#     (same path convention as `store materialize`), so the label symlink the manifest references is
#     FRESH the instant POST /publish returns -- otherwise the off-box deploy script's `supervisor
#     restart` re-reads the OLD symlink = a silent stale (old-code) deploy. Omit ONLY if publishd is
#     NOT on the acceptor's box (then that box needs its own materialize trigger).
```

## Endpoint (Bearer-authed unless noted)
```
GET  /content/<hash>  (or /by-hash/<hash>)  -> 200 blob (NO auth) | 404    [boot-from-store]
    PUBLIC, but GATED: streams the blob from the co-located holder ONLY if a LIVE index label under
    `--public-prefix` (default `wasm/`) points at that hash. So any box can boot-from-store over the
    public :18443 by content-hash (no GitHub, no store node) -- while SECRETS in the same CAS (labels
    outside the prefix) are NEVER publicly served, even if their hash is known. `--public-prefix ""`
    serves any hash (use ONLY if the store holds no secrets).
POST /publish?name=<label>     body = raw wasm bytes
    -> 200 {"name":"<label>","hash":"<64hex>"}   (compare hash to your local sha256 -> fail-loud on mismatch)
    -> 400 empty/short body (Content-Length not satisfied)   413 over --max-body-mb
    -> 401 bad/absent token    502 upstream (holder/index) error

DELETE /publish?name=<label>   -> 200 {"name":"<label>","removed":true}   (tombstone: deprecate/clean a label)
    once tombstoned, its content is gc-eligible (no live index entry). 401 bad token · 502 upstream.
GET  /resolve?name=<label>     (Bearer) -> 200 <64hex>\n | 404 name not in index   (deploy propagation gate)
    NOTE: /resolve IS authed too -- returns 401 without the token. ONLY /health is open.
GET  /health                   -> 200 ok          (no auth -- the only open route)
```

## Deploy script shape (inbox-dev's piece 4)
```sh
h=$(curl -fsS --cacert cert.pem -X POST "https://<publishd>/publish?name=inbox_acceptor" \
      -H "Authorization: Bearer $TOKEN" --data-binary @acceptor.wasm | jq -r .hash)
[ "$h" = "$(sha256sum acceptor.wasm | awk '{print $1}')" ] || { echo "integrity mismatch"; exit 1; }
# spine materializes name->local symlink; then:
supervisor restart inbox_acceptor        # current_id rotates + /version == build (fail-loud) = the backstop
```

## Hardening (built in / operator)
- **Auth:** constant-time bearer-token compare; token from `--token-file` (rotate = replace the file + restart).
- **Body drain:** reads EXACTLY `Content-Length` bytes in a loop; a short read → 400 (never authors a
  truncated/corrupt wasm — the #82 single-read truncation bug class, avoided).
- **Limits:** `--max-body-mb` (default 64) → 413; request-header cap 16 KiB → 431.
- **TLS:** rustls (ring), server cert/key. Callers verify with `--cacert cert.pem` (self-signed pin) or
  front it with a real cert. Operator should add rate-limiting at the front if exposed broadly.

## Proven (local E2E)
POST /publish (token + real wasm) → returned hash == local sha256 (integrity); the `Put` landed in the
real index (CLI `resolve` confirms) + content on the holder (materialize round-trip); GET /resolve →
correct hash; bad token → 401; /health → 200. Static-musl binary re-verified (`/health` 200).

## Build
Static-musl (statically linked, ~2.2 MB). `store-publishd` is a member of the repo-root cargo workspace;
`nix build .#default` at the repo root builds it + `store` together (nixos-25.05 pkgsStatic; `perl` for
ring). Depends on the shared `store-protocol` workspace crate; the mesh/content client is inlined here
(the wire protocol is fixed).
