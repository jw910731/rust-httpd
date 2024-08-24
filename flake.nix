{
  inputs = {
    nixpkgs.url = "github:cachix/devenv-nixpkgs/rolling";
    systems.url = "github:nix-systems/default";
    devenv.url = "github:cachix/devenv";
    devenv.inputs.nixpkgs.follows = "nixpkgs";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs = { nixpkgs.follows = "nixpkgs"; };
  };

  nixConfig = {
    extra-trusted-public-keys = "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw=";
    extra-substituters = "https://devenv.cachix.org";
  };

  outputs = { self, nixpkgs, devenv, systems, fenix, ... } @ inputs:
    let
      forEachSystem = nixpkgs.lib.genAttrs (import systems);
      musl-overlay = final: prev: {
        musl = prev.musl.overrideAttrs (old: {
          patches = (old.patches or []) ++ (prev.lib.optional (prev.stdenv.buildPlatform.isDarwin) (builtins.fetchurl {
              url = "https://github.com/timbertson/musl/compare/f314e133929b6379eccc632bef32eaebb66a7335...05b89f783fd1873ce9ec1127fa76d002921caa23.patch";
              sha256 = "1n17lawfpd551707nh3pr6ilyh0qh7rh0vdb522ijdygggh49rhd";
            })
          );
        });
      };
    in
    {
      packages = forEachSystem (system: 
      let
        pkgs = import nixpkgs { inherit system; overlays = [ musl-overlay ]; };
        lib = pkgs.lib;
        linuxPkgs = {
          "x86_64" = pkgs.pkgsCross.musl64.pkgsStatic;
          "aarch64" = pkgs.pkgsCross.aarch64-multiplatform-musl.pkgsStatic;
        };
        archName = {
          "x86_64" = "amd64";
          "aarch64" = "arm64";
        };
        rust-httpd-gen = linuxPkgs: let 
          target = (lib.systems.parse.tripleFromSystem linuxPkgs.stdenv.hostPlatform.parsed);
          toolchain = with fenix.packages.${system}; combine [
            stable.cargo
            stable.rustc
            targets.${target}.stable.rust-std
          ];
        in (linuxPkgs.makeRustPlatform {
          cargo = toolchain;
          rustc = toolchain;
        }).buildRustPackage {
          pname = "rust-httpd";
          version = "0.1.0";

          src = with lib.fileset; toSource {
            root = ./.;
            fileset = unions [
              ./src
              ./status_pages
              ./Cargo.lock
              ./Cargo.toml
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          logLevel = "info";
        };
        docker = linuxPkgs: pkgs.dockerTools.buildLayeredImage {
          name = "registry.h.jw910731.dev/nix/rust-httpd";
          tag = "0.1.0-" + archName.${linuxPkgs.stdenv.hostPlatform.parsed.cpu.name};
          contents = [
            (rust-httpd-gen linuxPkgs)
            linuxPkgs.busybox
          ];
          config = {
            Entrypoint = [ "${(rust-httpd-gen linuxPkgs)}/bin/rust-httpd" "0.0.0.0:80" ];
            Env = [
              "RUST_LOG=info"
            ];
            WorkingDir = "/";
          };
          created = "now";
          maxLayers = 127;
        };
      in {
        devenv-up = self.devShells.${system}.default.config.procfileScript;
        docker = (docker linuxPkgs.${pkgs.stdenv.hostPlatform.parsed.cpu.name});
      } // (lib.mapAttrs' (name: value: lib.nameValuePair ("docker-" + archName.${name}) (docker linuxPkgs."${name}")) linuxPkgs)
      );

      devShells = forEachSystem
        (system:
          let
            pkgs = nixpkgs.legacyPackages.${system};
          in
          {
            default = devenv.lib.mkShell {
              inherit inputs pkgs;
              modules = [
                {
                  # https://devenv.sh/reference/options/
                  packages = [ ];
                  languages.rust = {
                    enable = true;
                    channel = "stable";
                  };
                }
              ];
            };
          });
    };
}
