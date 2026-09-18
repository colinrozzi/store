# inbox-writer — the store WRITER node for inbox-dev's container (self-service deploy, piece 1)

Packages a store **writer peer** (index node + content holder) to run inside inbox-dev's container so
inbox-dev publishes deploys locally — `store publish` authors `name -> hash` on the index, the write
gossips to the prod cluster over the mesh, and consuming boxes `materialize` the label to a local path.
This is piece 1 of the team-locked self-service deploy (mail id=62/67). No manager in the routine loop.

## The deploy model (locked, id=67)
LABELS are **local symlinks**: `/var/lib/store/by-name/<name>.wasm -> /var/lib/store/packages/<sha256>.wasm`,
rewritten by `store materialize` on each deploy. Manifests reference the **stable symlink path** with
`static_package=false`, so theater `tokio::fs::read`s THROUGH it fresh every spawn (theater-dev confirmed
follow-symlinks) — no per-spawn network fetch, boot stays GitHub-independent, and a repoint needs **no
manifest change**. Deploy trigger = in-process `supervisor restart <acceptor>` (inbox-dev's :9000 lever).

## Files
- `store` — the static-musl CLI (feature-complete: init/publish/materialize/resolve/gc/add-writer/**pubkey**;
  materialize emits the label symlink). Statically linked, ~1 MB, zero deps — scp/copy into the container.
- `inbox-index.toml` / `inbox-holder.toml` — the writer's two manifests (identity + dial placeholders).
- `mesh_store.wasm` / `content_node.wasm` — the node wasms (`package` = `/etc/store/<wasm>`).
- `publish.sh` — the deploy publish step (walks an actor->wasm map, `store publish` each).

## Identity (baked)
seed `store-inbox-writer` -> pubkey `d03bb0f97b56786cf870c33644fbc4300d458bbb0b514e39b6e730de401bc9d0`
(verify: `./store pubkey --seed store-inbox-writer`). This is the pubkey to **allow-list** as a writer.

## Transport = encrypted static tunnel (b', Colin id=77) — NOT wireguard
`store-tunnel` (this dir, and `../store-tunnel/`) is the static TLS proxy. Curl it into the running
container (like the supervisor binary — no recreate). The node dials LOCAL tunnel endpoints; the client
tunnel carries each to a cluster peer over pinned TLS. The cluster mesh ports stay private on the VPS.
```sh
# manager ships cert.pem (pins the VPS proxy). Then in the container:
./store-tunnel client --ca cert.pem --name store-proxy \
  --route 127.0.0.1:9700=<PEER1_PUBLIC>:19700 \
  --route 127.0.0.1:9701=<PEER2_PUBLIC>:19700 \
  --route 127.0.0.1:9702=<PEER3_PUBLIC>:19700 \
  --route 127.0.0.1:9710=<PEER1_PUBLIC>:19710   # a holder route for publish
```
inbox-index.toml already dials 127.0.0.1:9700/9701/9702 (the local tunnel endpoints); publish uses
`--holder 127.0.0.1:9710`. mesh-dev: gossip is bidirectional over the one outbound dial, no inbound.

## Bring up (in inbox-dev's container)
1. Start the client tunnel (above), pinned to the manager's cert. Adjust the `--route` map + the
   manifest dial pubkey set to the ACTUAL prod cluster (2-node now: anchor `1900e667…`/edge `a2b26839…`;
   3-node HA: peer1/2/3, baked here).
2. Place the wasms at `/etc/store/`, data dir `/var/lib/store-inbox-writer/`.
3. Spawn: `theater spawn inbox-index.toml` + `theater spawn inbox-holder.toml`.

## Admit the writer (GATE — pick one, manager sequences)
The live cluster's index was already genesis'd, and genesis is **one-shot**, so admit this writer via
the mutable-membership path (recommended) or a fresh genesis:
- **(a) AddWriter [recommended]:** an existing allow-listed writer authors it — no re-genesis, one command:
  `./store add-writer --index <cluster> --pubkey d03bb0f97b56786cf870c33644fbc4300d458bbb0b514e39b6e730de401bc9d0`
  (or `--seed store-inbox-writer`). Pure set-add on the allow-list: idempotent + LWW-safe. Requires the
  `store_sm` on the cluster to include `Cmd::AddWriter` (committed 4c00540) — a coordinated recompose+redeploy
  the manager sequences. See `../DEPLOYMENT-DISTRIBUTION.md`.
- **(b) fresh genesis:** re-genesis the cluster with the writer in `--allow` (disruptive; only if standing
  the cluster up fresh anyway).

## Publish a deploy (from the container)
```sh
# addresses are the LOCAL tunnel endpoints (the client tunnel carries them to the cluster over TLS)
export INDEX=127.0.0.1:9700,127.0.0.1:9701,127.0.0.1:9702
export HOLDER=127.0.0.1:9710      # add ,127.0.0.1:9711,... if you tunnel more holder routes (RF)
./publish.sh ops/inbox-actors.json         # store publish each actor -> gossips to the cluster
```
Then, per consuming box (the VPS spine): `store materialize --name <actor> --root /var/lib/store
--index $INDEX --holder $HOLDER` repoints the label symlink to the new content, and inbox-dev's
`supervisor restart <actor>` picks it up (static_package=false -> fresh read). Freshness guard: poll
`supervisor status` for current_id rotation + curl the actor's /version endpoint == published build.

## Gating (manager-sequenced, prod-cluster-touching)
(i) wg transport — this container as a wg peer (fills the dial addrs).
(ii) `store_sm` AddWriter upgrade + admit pubkey `d03bb0f9…`.
(iii) [DONE] materialize emits label->local-symlink — confirmed/proven store-side (commit 180c3e4).
(iv) #5 manifest cutover to labels + `static_package=false` everywhere — Colin-gated (prod-mail boot change).
