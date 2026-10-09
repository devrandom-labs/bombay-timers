{
  description = "Bombay Timers — publish-ready Rust crate";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };
  outputs =
    {
      self,
      nixpkgs,
      utils,
      crane,
      fenix,
      advisory-db,
      ...
    }:
    utils.lib.eachSystem [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ] (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        rustToolchain = fenix.packages.${system}.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-mvUGEOHYJpn3ikC5hckneuGixaC+yGrkMM/liDIDgoU=";
        };
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
        src = pkgs.lib.cleanSource ./.;
        commonArgs = {
          inherit src;
          pname = "bombay-timers";
          version = (builtins.fromTOML (builtins.readFile ./crates/timers/Cargo.toml)).package.version;
          strictDeps = true;
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      in
      {
        checks = {
          timers-build = craneLib.buildPackage (commonArgs // { inherit cargoArtifacts; });
          timers-fmt = craneLib.cargoFmt { inherit src; };
          timers-clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
            }
          );
          timers-nextest = craneLib.cargoNextest (commonArgs // { inherit cargoArtifacts; });
          timers-doctest = craneLib.cargoDocTest (commonArgs // { inherit cargoArtifacts; });
          timers-doc = craneLib.cargoDoc (commonArgs // { inherit cargoArtifacts; });
          timers-audit = craneLib.cargoAudit { inherit src advisory-db; };
          timers-deny = craneLib.cargoDeny { inherit src; };
        };
        packages = {
          default = self.checks.${system}.timers-build;
          coverage = craneLib.cargoLlvmCov (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoLlvmCovCommand = "test";
              cargoLlvmCovExtraArgs = "--html --output-dir $out";
            }
          );
        };
        formatter = pkgs.nixfmt;
        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = with pkgs; [
            cargo-audit
            cargo-deny
            cargo-llvm-cov
            cargo-nextest
            git
            gh
            just
            nixfmt
          ];
        };
      }
    );
}
