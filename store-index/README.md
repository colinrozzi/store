# store-index — the mutable `name → hash` index (Layer 1)

The consensus half of the store (see `../LAYER0-DESIGN.md` §1): the mutable
`name → content-hash` index, implemented as an RSM **`state-machine`** component that
composes into the mesh node via `mesh.lib.mkComposite { name = "store"; sm = store_sm.wasm }`,
exactly like `bank-sm`. The mesh replicates only these small pointers — never a blob.

- **`store-protocol/`** — the typed schema (`#[derive(GraphValue)]`): payload `Cmd`
  (`Genesis` / `Put` / `Remove`) and state `IndexState`. The state is a CRDT **LWW
  register per name**: each `Entry` carries the `(ts, author)` that last set it, so the
  fold is order-independent.
- **`store-sm/`** — the SM: `initial-state` / `validate` / `apply` / `members`.
  - **write-auth:** `validate` admits `Put`/`Remove` only if the event's `author` (the
    signing **node** pubkey) is in an allow-list folded from a one-shot `Genesis` event
    — the control-plane `command_allow` shape.
  - **apply:** an LWW upsert — writes only if the incoming `(ts, author)` strictly beats
    the stored slot, so concurrent same-name writes converge from event content, not fold
    position (the confluence the mesh contract requires).

## Status — GREEN (SM logic proven + composes)

- `store_sm.wasm` builds under nix (composes with the mesh node's `state-machine` ABI).
- **`cargo test` — 12/12 green**, the driven proof (host fold, like `bank-sm`):
  - `store-protocol` (3): `Cmd`/state round-trip through the Graph ABI + canonical order.
  - `store-sm` (9): authorized Put admitted · unauthorized rejected · write-before-genesis
    rejected · malformed-hash rejected · **concurrent same-name converges regardless of
    fold order** · ts-tie breaks on author · remove/re-add deterministic · different names
    commute · genesis sorts+dedups the allow-list.

Not yet: composing on a live mesh network + driving real `Put`s over `my:mesh.*` (needs
mesh-system + a driver — the next step); and cold-boot persistence (D1-b, mesh-dev owns —
the store's production acceptance bar). See `../LAYER0-DESIGN.md`.

## Build + test

```sh
# wasm (composes with the mesh): reuse the mesh flake's buildWasm.
MESH=<mesh flake source in /nix/store>   # a dir with node.pact + flake.nix
nix build --impure --expr '
  let mesh = builtins.getFlake "path:'"$MESH"'";
  in mesh.lib.x86_64-linux.buildWasm {
    pname = "store-sm"; src = ./.; crate = "store-sm"; wasmName = "store_sm.wasm";
  }' -o result

# logic proof (host): a rust toolchain + a cc-wrapper on PATH, then
cargo test --manifest-path store-protocol/Cargo.toml
cargo test --manifest-path store-sm/Cargo.toml
```

> Env note: this container aggressively GCs the nix store, so a fresh build may need
> binary substitution (drop `--offline`) to re-fetch a GC'd stdenv; and the mesh source
> store path changes across GCs — re-resolve it (a dir with `node.pact` + `flake.nix`).
