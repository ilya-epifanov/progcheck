{
  description = "Development environment for progcheck";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      crane,
      flake-utils,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        craneLib = crane.mkLib pkgs;

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "rustfmt"
          ];
        };

        hooksmith =
          let
            hooksmithSrc = pkgs.fetchFromGitHub {
              owner = "TomPlanche";
              repo = "hooksmith";
              rev = "v1.13.0";
              hash = "sha256-03EXvJctt/Ro27rna7DrCR1IdxIH2kFEQobSbK84p0s=";
            };
          in
          craneLib.buildPackage {
            src = hooksmithSrc;
            strictDeps = true;
            doCheck = false;
          };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.python3
            pkgs.just
            pkgs.cargo-watch
            pkgs.cargo-nextest
            pkgs.cargo-edit
            pkgs.pkg-config
            pkgs.cargo-dist
            hooksmith
          ];
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
