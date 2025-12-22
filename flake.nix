{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
  };

  outputs = {nixpkgs, ...}: let
    inherit (nixpkgs) lib;

    forEachSystem = fn: lib.genAttrs lib.systems.flakeExposed (system: fn system nixpkgs.legacyPackages.${system});
  in {
    formatter = forEachSystem (system: pkgs:
      pkgs.writeShellApplication {
        name = "aljd";
        runtimeInputs = with pkgs; [alejandra fd];
        text = ''
          fd "$@" -t f -e nix -X alejandra -q '{}'
        '';
      });

    packages = forEachSystem (system: pkgs: {
      default = pkgs.callPackage ./package.nix {};
    });

    devShells = forEachSystem (system: pkgs: {
      default = pkgs.mkShell {
        packages = with pkgs; [rustc cargo clippy rustfmt];
      };
    });
  };
}
