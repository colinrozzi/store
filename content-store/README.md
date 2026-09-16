# content-store — the store's immutable CAS layer (Layer 2)

The immutable `hash -> bytes` half of the store (see `../DESIGN.md`). A thin,
provable wrapper over theater's `theater:simple/store` handler (already a
content-addressed store with dedup), exposing the deliberately-dumb primitive:

| op | via |
|----|-----|
| `put(bytes) -> hash` | `store::store` |
| `get(hash) -> bytes` | `store::get` |
| `has(hash) -> bool`  | `store::exists` |

The mutable `name -> hash` index (Layer 1, a mesh app-SM) sits on top of this and
is the GC root. This layer needs no consensus (the key *is* the hash) and is
distributed fleet-wide later by fetch-by-hash — none of which is in this crate yet.

## Status — GREEN (built + tested locally under nix + theater)

`init` runs a self-test against the real host store and shuts down with a
`content-store-passed` / `content-store-failed:<reason>` marker. Proven:

```
put(A) -> 1aed123f…76a6e (40 hex chars)
get(hash_a) round-trips: OK
has(present)=true, has(absent)=false: OK
put(A) again -> same hash: OK                 # dedup: identical bytes -> same hash
put(B) -> 8f9f22aa…7f8bd (distinct): OK
total size 34 bytes == len(A)+len(B): OK       # dedup is real: A stored once
=== all CAS tests passed ===
```

## Build + run recipe (what actually works in this container)

The container has no source checkouts of theater/pack/mesh (they're `git+file://`
flake inputs pointing at absent paths), but their sources + a matching rust
toolchain are realized in the nix store, and `packr-guest` is on crates.io. So:

Build the wasm by reusing the mesh flake's `buildWasm` (rust wasm toolchain +
offline vendoring — needs only the toolchain, not the theater/packr CLIs):

```sh
MESH=$(nix eval --raw ...)   # the mesh flake source in /nix/store (dp39…-source here)
nix build --impure --offline --expr '
  let mesh = builtins.getFlake "path:'"$MESH"'";
  in mesh.lib.x86_64-linux.buildWasm {
    pname = "content-store"; src = ./.; crate = "."; wasmName = "content_store.wasm";
  }' -o result
```

Run it under a theater CLI (a prebuilt `theater 0.3.17` from the store works;
`theater spawn` runs a local in-process runtime — no server):

```sh
mkdir -p run/target/wasm32-unknown-unknown/release
cp result/content_store.wasm run/target/wasm32-unknown-unknown/release/
cp manifest.toml run/
cd run && theater spawn manifest.toml --log-level warn
```

## Toolchain findings (this container)

- **pack lineage:** two coexist in-store — old `pack-guest`/`pack_guest` 0.1.0
  (theater's in-tree examples) and current **`packr-guest`/`packr_guest` 0.15**
  (crates.io; what the mesh SMs build against). This crate uses `packr-guest 0.15`
  to share one toolchain lineage with the future mesh index SM.
- **`packr-guest` is on crates.io** — no source vendoring needed; `packr-guest = "0.15"`.
- **runtime cap is Disallow-by-default.** Without `[permission_policy.runtime]
  type = "inherit"` in the manifest, `theater:simple/runtime::{log,shutdown}` are
  never registered and instantiation fails with `unknown import: …runtime::log`.
- **`pack_types!` metadata is REQUIRED.** Theater rejects any actor without an
  embedded `__pack_types` export ("no interface metadata"). Declare the host
  functions you import (a subset of an interface is fine) + the `actor.init`
  export; signatures must mirror theater's `runtime.pact` / `store.pact`.
- **digest is SHA-1, not SHA-256.** theater's store substrate emits a 40-char
  SHA-1 content ref here, despite the `store.wit` docstring saying SHA-256. The
  CAS contract (round-trip + dedup) is digest-agnostic; the digest is a Layer-2
  choice the fleet store can pin.
- **theater version:** prebuilt CLIs top out at 0.3.18; 0.3.17 runs a standalone
  `packr-guest 0.15` actor cleanly. 0.3.18 has a runtime-handler quirk that
  suppresses `log`. The mesh's own theater (rev 307fa35 ≈ 0.3.26) can't be built
  offline here (its flake source is filtered and some deps need network).
