{
  # Builds the store's NATIVE tools (the cargo workspace) as static-musl binaries: `store` (store-cli)
  # + `store-publishd`. The wasm actors (store-sm, content-node) build separately via the mesh's
  # buildWasm -- see store-index/README.md + content-node/README.md.
  description = "store native tools (store-cli + store-publishd), static-musl";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in {
      packages.${system}.default = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
        pname = "store-tools";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.perl ];   # ring (store-publishd TLS) needs perl at build time
        doCheck = false;
      };
    };
}
