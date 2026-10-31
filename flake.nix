{
  # A developer shell with everything a clean clone needs to build neochumby
  # and test it on the emulator, the same on macOS and Linux (NixOS too):
  #
  #   nix develop            # then: cargo chumby, tools/tree, dev/e2e ...
  #
  # Rust (stable, with the Chumby's, the server's and 64-bit ARM's musl
  # targets, and wasm32-wasip1), zig 0.17 (the C compiler, linker and musl,
  # as CI pins it), CMake (for fetch and WAMR), libclang (WAMR's bindings),
  # Python 3 (dev/emulate and friends), gh (dist/fetch verifies the prebuilt
  # images' attestations with it) and jq. QEMU and the Chumby's Linux images
  # aren't built: dev/emulate fetches qemu-chumby's prebuilt QEMU and CI's
  # images, verified. Without Nix, CONTRIBUTING.md lists the same tools, and
  # dev/setup says which are missing.
  description = "neochumby: a developer shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      }));
    in
    {
      devShells = forAll (pkgs:
        let
          rust = pkgs.rust-bin.stable.latest.default.override {
            extensions = [ "clippy" "rustfmt" "rust-src" ];
            targets = [
              "armv5te-unknown-linux-musleabi"
              "x86_64-unknown-linux-musl"
              "aarch64-unknown-linux-musl"
              # dev/wasmbench's workloads, and wasm apps (proposal 0003).
              "wasm32-wasip1"
            ];
          };
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              pkgs.zig_0_17
              pkgs.cmake
              pkgs.python3
              pkgs.gh
              pkgs.jq
              pkgs.xz
              pkgs.git
            ];
            # wamr-sys generates WAMR's bindings with bindgen, which loads
            # libclang; nixpkgs' doesn't find its own builtin headers.
            LIBCLANG_PATH = "${pkgs.llvmPackages_18.libclang.lib}/lib";
            BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.llvmPackages_18.libclang.lib}/lib/clang/18/include";
            # The Chumby's: clang's own headers only (.cargo/config.toml).
            BINDGEN_EXTRA_CLANG_ARGS_armv5te_unknown_linux_musleabi = "-nostdlibinc -isystem ${pkgs.llvmPackages_18.libclang.lib}/lib/clang/18/include";
            # tools/zigcc and its siblings call zig by name; the macOS SDK
            # Nix's stdenv puts in the environment would steer zig's own
            # cross compilation, so leave zig to find what it needs.
            shellHook = ''
              unset SDKROOT DEVELOPER_DIR
            '';
          };
        });
    };
}
