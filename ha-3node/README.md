# 3-node HA store network (productionization)

Three EQUAL peers, full-mesh, RF=3 — the productionized distribution network. See
`../DEPLOYMENT-DISTRIBUTION.md` "3-node HA" for the topology + HA analysis. Each peer runs an
**index node** (persist ON) + a **content holder** (persist ON, serves http). All artifacts here;
fill the placeholders and spawn.

## Fill in before running
- `<abs path>/mesh_store.wasm`, `<abs path>/content_node.wasm` — this dir's wasms (absolute path).
- `<PEERn_WG_IP>` — the wireguard address of each peer (from the manager's WG mesh). Ports:
  index `:9700`, content http `:9710`.
- `<PEERn_SEED>` — the RANDOM high-entropy node seed, kept ONLY in `seeds.env` (gitignored, on-box,
  NEVER committed). Render it into the manifests at deploy (below). The dial lists already carry the
  peers' PUBLIC pubkeys (safe to commit; pubkey = ed25519(sha256(seed)), infeasible to reverse).

Files: `peerN-index.toml` + `peerN-holder.toml` (N=1,2,3), `seeds.env.example` (template — copy to the
gitignored `seeds.env` on-box and fill real random seeds).

## Build the artifacts (not committed — public repo keeps binaries out of git)
The binaries + wasms are build outputs, gitignored; build and drop them here (or `/etc/store/`) at deploy:
- `store` (static-musl CLI) — from `../store-cli` (`cargo build --release`, or the static-musl nix
  pattern in `../store-publishd/flake.nix`).
- `mesh_store.wasm` — the mesh⊕index composite, `mesh.lib.mkComposite { name="store"; sm=store_sm.wasm }`
  over `../store-index/store-sm` (see `../store-index/README.md`).
- `content_node.wasm` — from `../content-node` (see its README).

## Render the seeds at deploy (placeholders -> real, from the on-box secrets)
```sh
set -a; . ./seeds.env; set +a          # PEER1_SEED.. (random, gitignored, never in the repo)
for n in 1 2 3; do
  eval "s=\$PEER${n}_SEED"
  sed "s/<PEER${n}_SEED>/$s/" peer${n}-index.toml > /etc/store/peer${n}-index.toml   # + fill <PEERn_WG_IP>
done
# holders carry no seed. Real seeds never touch the tracked repo.
```

## Bring up (each peer, on its box)
```sh
theater spawn peerN-index.toml    # index node: persist ON, full-mesh dial (the other two peers)
theater spawn peerN-holder.toml   # content holder: persist ON, http + content wire
```
The `dial` list in each index manifest already names the other two peers. mesh-dev's
self-healing dial makes those dials **persistent + self-reconnecting** (re-dials any zero-conn
peer on tick) — transparent to this config; it just makes the full mesh heal after a drop.

## Genesis (once) — pick single- or multi-writer
```sh
# single-writer (v1, simplest): only peer-1 may author index writes
./store init --index <PEER1_WG_IP>:9700 --node-seed <PEER1_SEED>

# multi-writer (WRITE-HA): any peer may author; the LWW register converges failover writes,
# NO consensus. Recommended for HA.
./store init --index <PEER1_WG_IP>:9700,<PEER2_WG_IP>:9700,<PEER3_WG_IP>:9700 \
             --allow <PEER1_SEED>,<PEER2_SEED>,<PEER3_SEED>
```
Keep ONE logical publisher (the release pipeline); multi-writer is for FAILOVER, not concurrent
independent publishing.

## Publish (replicates to all holders = RF=3; index endpoints fail over)
```sh
./store publish --name wasm/<actor> --wasm <file> \
  --holder <PEER1_WG_IP>:9710,<PEER2_WG_IP>:9710,<PEER3_WG_IP>:9710 \
  --index  <PEER1_WG_IP>:9700,<PEER2_WG_IP>:9700,<PEER3_WG_IP>:9700
# -> pushes the bytes to all 3 holders (durability), authors name->hash via the first live index peer.
```

## Consume on a box
```sh
# #2 materialize (verified + boot-local, prod mode): reads from any live index + holder
./store materialize --name wasm/<actor> --root /var/lib/store \
  --index  <PEER1_WG_IP>:9700,<PEER2_WG_IP>:9700,<PEER3_WG_IP>:9700 \
  --holder <PEER1_WG_IP>:9710,<PEER2_WG_IP>:9710,<PEER3_WG_IP>:9710
# -> /var/lib/store/packages/<sha>.wasm (verified); roster manifest package = that local path.

# #1 http boot-pull (dev/light): consumer manifest package = http://<any_live_peer>:9710/by-hash/<sha>
```
`--index`/`--holder` take comma lists → automatic read/publish failover across live peers.

## What this delivers (with 3 DISTINCT machines + self-healing dial + WG)
- Survive ANY 1 machine loss with **reads + content + writes** (multi-writer) intact.
- Survive 2 machine losses for **reads + content** (writes need a live allow-listed peer).
- Content RF=3 (each blob on all 3); index folds locally on each (read-HA).
- 3 nodes on 2 boxes only survives 1 BOX loss — true machine-loss HA needs 3 distinct machines.

## Division
- **store-dev:** these manifests + the CLI failover/RF (done).
- **mesh-dev:** self-healing dial (persistent + multi-peer re-dial) — prerequisite for the mesh HA.
- **manager:** the wireguard mesh (durable transport) replacing the ad-hoc ssh -R.
- **supervisor-dev:** host these as supervisor roster entries (crash-restart + reboot-durable).

## Deploy convention (supervisor-dev, adopted)
Per box, under `/etc/store/`: the WG-filled `peerN-index.toml` + `peerN-holder.toml`, the two node
wasms (`mesh_store.wasm`, `content_node.wasm`), and the static `store` CLI. The manifest `package`
fields point at `/etc/store/<wasm>` (concrete above); persistent data lives at `/var/lib/store-node-N/`.
supervisor-dev's `deploy/ha-3node/peerN-roster.json` hosts that peer's {index, holder} (crash-restart,
breaker bumped to 10/60s, keep_chain) via the `store-supervisor@` systemd unit (Restart=always +
boot-enable = reboot-durable). Only `<PEERn_WG_IP>` remains to fill (from the manager's wireguard mesh).
