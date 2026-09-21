# 3-node HA store — consolidated deploy runbook

Step-by-step companion to `README.md` (topology + HA analysis). This dir holds everything for the deploy
except secrets + build artifacts (both kept out of git):

  peer{1,2,3}-index.toml   peer{1,2,3}-holder.toml   # node manifests (package=/etc/store/<wasm>)
  seeds.env.example                                  # template -> copy to seeds.env (gitignored) on-box
  README.md · DEPLOY.md (this)

Built + placed at deploy (NOT committed — see README "Build the artifacts"):
  store (static CLI) · mesh_store.wasm (mesh⊕index composite) · content_node.wasm

Prereqs the manager supplies: the transport (the deploy interface is `store-publishd`; the cluster mesh
ports stay loopback-private) + reachable inter-peer addresses (WG or equivalent) for `<PEERn_WG_IP>`
+ Colin's 3rd-machine call for true machine-loss HA.

## Per box (peer N), as root
```sh
install -d /etc/store /var/lib/store-node-$N
# 1. place the BUILT artifacts + this peer's manifests
cp mesh_store.wasm content_node.wasm store /etc/store/ && chmod +x /etc/store/store
# 2. render the RANDOM node seed into the manifest from the on-box secrets (seeds.env is gitignored)
set -a; . ./seeds.env; set +a
eval "s=\$PEER${N}_SEED"
sed "s/<PEER${N}_SEED>/$s/; s/<PEER${N}_WG_IP>/<this peer's addr>/g" peerN-index.toml > /etc/store/peerN-index.toml
sed "s/<PEER${N}_WG_IP>/<this peer's addr>/g" peerN-holder.toml > /etc/store/peerN-holder.toml
# (fill the OTHER peers' <PEERm_WG_IP> in the dial list too; pubkeys are already baked)
```

## Genesis (ONCE, from any box with CLI reachability)
```sh
# multi-writer (write-HA): any peer may author; the LWW register converges failover writes, no consensus.
store init --index <P1>:9700,<P2>:9700,<P3>:9700 \
  --allow "$PEER1_SEED,$PEER2_SEED,$PEER3_SEED"          # seeds from seeds.env; NEVER hard-code them
# single-writer (simplest): store init --index <P1>:9700 --node-seed "$PEER1_SEED"
```
Keep ONE logical publisher (the release pipeline); multi-writer is for FAILOVER, not concurrent publishing.

## Bring up (plain systemd — recommended)
One templated unit, instantiated per manifest (`Restart=always`, boot-enabled = reboot-durable). The
full supervisor was stood down as over-provisioned for a fixed cluster (no dynamic membership to
reconcile).
```ini
# /etc/systemd/system/store-node@.service
[Unit]
Description=store cluster node %i
After=network-online.target
Wants=network-online.target
[Service]
ExecStart=/etc/store/theater spawn /etc/store/%i.toml   # adjust to wherever the theater binary lives
Restart=always
RestartSec=2
[Install]
WantedBy=multi-user.target
```
```sh
# enable the 6 instances (3 peers x index+holder):
systemctl enable --now store-node@peer1-index store-node@peer1-holder \
  store-node@peer2-index store-node@peer2-holder \
  store-node@peer3-index store-node@peer3-holder
```
`theater spawn` runs foreground, so `Type=simple` supervises it directly. A peer's process death takes
down only that node; RF=3 keeps ≥2/3 serving.

Two caveats:
- **Not machine-loss HA:** 3 nodes on ONE box is crash-resilience only. True HA needs a 3rd DISTINCT
  machine + the WG mesh (Colin's call).
- **Startup dial races:** on a cold bring-up the peers dial each other before all are listening — a clean
  bring-up order, or mesh-dev's self-healing dial (re-dials any zero-conn peer on tick), settles it.

## Publish / consume
```sh
# deploy interface: store-publishd (token-authed HTTPS front, co-located with a peer) -- see ../store-publishd.
# direct CLI (from any box with CLI + reachability):
store publish --name wasm/<a> --wasm <f> \
  --holder <P1>:9710,<P2>:9710,<P3>:9710 --index <P1>:9700,<P2>:9700,<P3>:9700   # RF=3
store materialize --name wasm/<a> --root /var/lib/store \
  --index <P1>:9700,<P2>:9700,<P3>:9700 --holder <P1>:9710,<P2>:9710,<P3>:9710   # verified, boot-local
```
`--index`/`--holder` are comma lists → automatic failover across live peers.

## What this delivers / doesn't (honest boundary)
Per-box process resilience (crash-restart) + reboot durability. NOT cross-machine failover (a dead box's
process manager dies with it) — that's the store's replication (reads/content, RF=3) + the multi-writer
allow-list (write-HA). A respawned node rehydrates its index (node.resume) + content (store reopen), both
keyed on the manifest-pinned store, not the actor id. mesh-dev's self-healing dial heals isolated links.
True machine-loss HA needs 3 DISTINCT machines.
