# 3-node HA store — consolidated deploy (single directory)

This directory (`/work/actors/store/ha-3node/`, host-reachable via the bind-mount) now holds
EVERYTHING for the deploy — store-dev's node artifacts + supervisor-dev's rosters/unit:

  peer{1,2,3}-index.toml   peer{1,2,3}-holder.toml   # node manifests (package=/etc/store/<wasm>)
  mesh_store.wasm  content_node.wasm  store          # composite, content node, static CLI
  seeds.txt                                          # 3 peer identities + pubkeys
  peer{1,2,3}-roster.json  store-supervisor@.service # supervisor-dev's rosters + systemd unit
  README.md (topology/HA) · DEPLOY.md (this)

Prereqs the manager supplies: the wireguard mesh (fill `<PEERn_WG_IP>` in each peerN-index.toml)
+ the `supervisor` binary at /usr/local/bin/supervisor + Colin's 3rd-machine call.

## Per box (peer N), as root
```sh
# 1. stage store files + rosters + unit
install -d /etc/store
cp peerN-index.toml peerN-holder.toml mesh_store.wasm content_node.wasm store peerN-roster.json /etc/store/
chmod +x /etc/store/store
cp store-supervisor@.service /etc/systemd/system/ && systemctl daemon-reload
# 2. fill the wireguard address in /etc/store/peerN-index.toml  (<PEERn_WG_IP> -> the WG IPs)
# 3. (peer-1 / whichever is the writer, ONCE) author genesis:
/etc/store/store init --index <PEER1_WG_IP>:9700 --node-seed store-peer-1
#    or multi-writer (write-HA): --index <all>:9700 --allow store-peer-1,store-peer-2,store-peer-3
# 4. bring the peer up SUPERVISED (crash-restart + reboot-durable):
systemctl enable --now store-supervisor@N
```

## Two bring-up paths (manager's call)
- **Deploy-fresh-supervised (recommended for the HA cluster):** the cluster isn't up yet, so
  bring each peer up directly under its `store-supervisor@N` unit — no plain-spawn step.
- **Migrate-in-place (only if supervising an already-running plain cluster):** ONE peer at a
  time — stop that peer's plain theater spawns, `systemctl enable --now store-supervisor@N`,
  verify it rejoined + mesh re-synced (RF=3 stays >=2/3 throughout), then the next.

## Publish / consume (from any box with the CLI + WG reachability)
```sh
store publish --name wasm/<a> --wasm <f> \
  --holder <P1>:9710,<P2>:9710,<P3>:9710 --index <P1>:9700,<P2>:9700,<P3>:9700   # RF=3
store materialize --name wasm/<a> --root /var/lib/store \
  --index <P1>:9700,<P2>:9700,<P3>:9700 --holder <P1>:9710,<P2>:9710,<P3>:9710   # verified, boot-local
```
`--index`/`--holder` are comma lists → automatic failover across live peers.

## What supervision does / doesn't (honest boundary)
Per-box process resilience (crash-restart, breaker 10/60s) + reboot durability (boot-enabled
unit). NOT cross-machine failover (a dead box's supervisor dies too) — that's the store's
replication (reads/content) + the multi-writer allow-list (write-HA). Verified: a respawned
node rehydrates index (node.resume) + content (store reopen), both keyed on the manifest-pinned
store, not the actor id. mesh-dev's self-healing dial heals live-but-isolated links.
