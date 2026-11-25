{
  description = "GlobalPTT";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {inherit system;};
      craneLib = crane.mkLib pkgs;

      src = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          (craneLib.filterCargoSources path type)
          || (pkgs.lib.hasInfix "/data/" path);
      };

      commonArgs = {
        inherit src;
        strictDeps = true;
        nativeBuildInputs = [
          pkgs.pkg-config
          pkgs.rustPlatform.bindgenHook
        ];
        buildInputs = [
          pkgs.pipewire.dev
          pkgs.libclang
        ];
      };

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      binary = craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          postInstall = ''
            install -D data/io.github.tofixrs.GlobalPTT.desktop \
              "$out/share/applications/io.github.tofixrs.GlobalPTT.desktop"
            substituteInPlace "$out/share/applications/io.github.tofixrs.GlobalPTT.desktop" \
              --replace-fail '@EXEC@' "$out/bin/GlobalPTT"
          '';
        });
    in {
      checks = {
        "GlobalPTT" = binary;
        "GlobalPTT-clippy" = craneLib.cargoClippy (commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });
        "GlobalPTT-doc" = craneLib.cargoDoc (commonArgs
          // {
            inherit cargoArtifacts;
          });
        "GlobalPTT-fmt" = craneLib.cargoFmt {inherit src;};
        "GlobalPTT-nextest" = craneLib.cargoNextest (commonArgs
          // {
            inherit cargoArtifacts;
            partitions = 1;
            partitionType = "count";
          });
      };

      packages.default = binary;

      apps.default = flake-utils.lib.mkApp {drv = binary;};

      devShells.default = craneLib.devShell (commonArgs
        // {
          checks = self.checks.${system};
          packages = with pkgs; [
            rustfmt
            clippy
            rust-analyzer
          ];
        });
    });
}
