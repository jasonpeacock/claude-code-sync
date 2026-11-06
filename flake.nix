{
  description = "Portable backup, restore and cross-machine sync for a Claude Code setup";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          claude-code-sync = pkgs.callPackage ./nix/package.nix { };
        in
        {
          inherit claude-code-sync;
          default = claude-code-sync;
        }
      );

      overlays.default = final: prev: {
        claude-code-sync = final.callPackage ./nix/package.nix { };
      };

      homeModules.default = import ./nix/hm-module.nix self;
      homeManagerModules = self.homeModules;

      checks = forAllSystems (pkgs: {
        inherit (self.packages.${pkgs.stdenv.hostPlatform.system}) claude-code-sync;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.claude-code-sync ];
          packages = [
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
