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
          patches = (old.patches or []) ++ [
            (builtins.fetchurl {
              url = "https://github.com/timbertson/musl/compare/f314e133929b6379eccc632bef32eaebb66a7335...05b89f783fd1873ce9ec1127fa76d002921caa23.patch";
              sha256 = "1n17lawfpd551707nh3pr6ilyh0qh7rh0vdb522ijdygggh49rhd";
            })
          ];
        });
      };
    in
    {
      packages = forEachSystem (system: 
      let
        pkgs = import nixpkgs { inherit system; overlays = [ musl-overlay ]; };
        lib = pkgs.lib;
        target = (lib.systems.parse.mkSystemFromString system).cpu.name + "-unknown-linux-musl";
        linuxCrossPkgs = if target == "x86_64-unknown-linux-musl"
                              then pkgs.pkgsCross.musl64 else
                               if target == "aarch64-unknown-linux-musl" then pkgs.pkgsCross.aarch64-multiplatform-musl 
                               else throw "Unsupported host platform";
        toolchain = with fenix.packages.${system}; combine [
          stable.cargo
          stable.rustc
          targets.${target}.stable.rust-std
        ];
        rust-httpd = (linuxCrossPkgs.makeRustPlatform {
          cargo = toolchain;
          rustc = toolchain;
        }).buildRustPackage {
          pname = "rust-httpd";
          version = "0.1.0";

          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;
        };
      in {
        devenv-up = self.devShells.${system}.default.config.procfileScript;
        docker = pkgs.dockerTools.buildImage {
          name = "rust-httpd";
          tag = "0.1.0";
          copyToRoot = pkgs.buildEnv {
            name = "image-root";
            pathsToLink = [ "/bin" ];
            paths = [rust-httpd];
          };
          config = {
            Cmd = [ "${rust-httpd}/bin/rust-httpd" ];
            Env = [];
          };
          created = "now";
        };
      });

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
