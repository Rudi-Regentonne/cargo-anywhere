{
  description = "cargo-anywhere: Nix/devenv-aware remote Cargo runner";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (import nixpkgs {
            inherit system;
          })
        );
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "cargo-anywhere";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;

          meta = with pkgs.lib; {
            description = "Nix/devenv-aware remote Cargo runner with automatic client CPU optimization";
            homepage = "https://github.com/Rudi-Regentonne/cargo-anywhere";
            license = licenses.mit;
            mainProgram = "cargo-anywhere";
          };
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            rsync
          ];
        };
      });
    };
}
