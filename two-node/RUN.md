# Two-node deployment-distribution run (store-dev drop)

Artifacts in this dir:
- `mesh_store.wasm` — the store index composite (mesh@febee526 ⊕ node ⊕ store-sm). Anchor+edge run this SAME wasm; the anchor/edge difference is 100% manifest.
- `content_node.wasm` — the content node (holds bytes, serves http `/by-hash/<sha>` + the binary content wire).
- `store` — the store CLI (init/publish/resolve/materialize). **static-musl** (fully static, no libc/nix deps) — scp it anywhere, incl. the VPS. Verified end-to-end (publish+materialize).
- `seeds.txt` — ANCHOR/EDGE seeds + pubkeys.

**theater:** built from `github:colinrozzi/theater?rev=00b0bf93fe69a231463d3ba918fa435c5f2a517d#default` (or your existing 00b0bf93 host theater). Spawn a node with: `theater spawn <manifest.toml>` (add `--log-level info` to watch). Run each node in its own process; it stays up (a listener).

**Addresses (SSH -R tunnel bridges both):** anchor listens `127.0.0.1:9700` (index) and `127.0.0.1:9710` (content http). On the VPS, `127.0.0.1:9700`/`:9710` reach the anchor over the tunnel. So every address below is `127.0.0.1` on both sides.

---

## ANCHOR (dev-box host — persist ON, listens, sole publisher)

`anchor-index.toml` (the index node):
```toml
name = "store-node-a"
version = "0.1.0"
package = "/abs/path/to/mesh_store.wasm"
static_package = true
initial_state = '{"node_seed":"<ANCHOR_SEED>","listen_addr":"127.0.0.1:9700"}'
[[handler]]
type = "self"
[[handler]]
type = "tcp"
[[handler]]
type = "timer"
[[handler]]
type = "message-server"
[[handler]]
type = "store"
base_path = "/var/lib/store-node-a"
store_id = "store"
```

`anchor-holder.toml` (the content node — serves the wasm over http):
```toml
name = "store-holder"
version = "0.1.0"
package = "/abs/path/to/content_node.wasm"
static_package = true
initial_state = 'role=server;listen=127.0.0.1:9710'
[[handler]]
type = "self"
[[handler]]
type = "tcp"
[[handler]]
type = "store"
base_path = "/var/lib/store-content"
store_id = "holder"
```

Spawn both (each in its own process), then:
```sh
# 1) GENESIS -- allow-list the anchor node as the sole writer (once):
./store init    --index 127.0.0.1:9700 --node-seed <ANCHOR_SEED>
# 2) PUBLISH the noop wasm (fetch it first from supervisor-dev's URL):
curl -sL https://raw.githubusercontent.com/colinrozzi/supervisor/main/experiments/noop/noop.wasm -o noop.wasm
./store publish --index 127.0.0.1:9700 --holder 127.0.0.1:9710 --name noop --wasm noop.wasm
#   -> prints: published noop (<N> bytes) -> <SHA256>   (that SHA is the by-hash id)
./store resolve --index 127.0.0.1:9700 --name noop      # sanity: prints <SHA256>
```

---

## EDGE (VPS — persist OFF, dials anchor, resyncs)

`edge-index.toml` (SAME wasm; NO store handler = persist OFF; dials the anchor):
```toml
name = "store-node-b"
version = "0.1.0"
package = "/abs/path/to/mesh_store.wasm"
static_package = true
initial_state = '{"node_seed":"<EDGE_SEED>","listen_addr":"127.0.0.1:9700","dial":[{"pubkey":"1900e667f84e437e94c6b5035121c1ae77a4eaedd5f37703ad6f9960f08a3aaf","address":"127.0.0.1:9700"}]}'
[[handler]]
type = "self"
[[handler]]
type = "tcp"
[[handler]]
type = "timer"
[[handler]]
type = "message-server"
```
Spawn it; it dials the anchor over the tunnel and resyncs the index. Verify:
```sh
./store resolve --index 127.0.0.1:9700 --name noop    # on the VPS -> must print the SAME <SHA256>
```
(Validated locally: the edge resolves a name the anchor authored — the index replicates cross-node.)

---

## Boot a test actor from the store

### #1 — HTTP boot-pull (works now, no CLI on the VPS)
On the VPS, a consumer manifest whose `package` is the anchor's http endpoint (reached over the tunnel):
```toml
name = "noop-http"
version = "0.1.0"
package = "http://127.0.0.1:9710/by-hash/<SHA256>"
[[handler]]
type = "self"
```
`theater spawn noop-http.toml` → theater http-pulls the wasm from the anchor store node (not GitHub) and instantiates noop. The holder logs `[http] 200 /by-hash/<SHA256>`.

### #2 — materialize (verified, network-free at boot; the static `store` runs on the VPS)
```sh
./store materialize --index 127.0.0.1:9700 --holder 127.0.0.1:9710 --name noop --root /var/lib/store
#   -> writes /var/lib/store/packages/<SHA256>.wasm  (fetched + SHA-256 verified)
```
Then a consumer manifest with `package = "/var/lib/store/packages/<SHA256>.wasm"` (a local path) → theater loads it with no spawn-time network. This is the prod mode (verified + boot-local).

---

## Done =
Publish on the anchor → the edge resyncs the index → a VPS test actor boots noop from the on-box
store (http #1 now; materialized-local #2 verified) — not from GitHub. Ping store-dev with the
`[http] 200` line / the spawned noop and I'll confirm.

---
## theater binary (concrete)
Rev **00b0bf93fe69a231463d3ba918fa435c5f2a517d**. Build: `nix build github:colinrozzi/theater?rev=00b0bf93fe69a231463d3ba918fa435c5f2a517d#default` -> `result/bin/theater`.
In store-dev's container it's realized at `/nix/store/ri7sckq7mjdq87qgchqjcwjnr32hq45s-theater-0.3.9/bin/theater` (a big dynamic nix binary -- rebuild on the host/VPS rather than scp).
