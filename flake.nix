{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    ...
  }: let
    inherit (nixpkgs) lib;

    forEachSystem = fn: lib.genAttrs lib.systems.flakeExposed (system: fn system nixpkgs.legacyPackages.${system});
  in {
    formatter = forEachSystem (system: pkgs:
      pkgs.writeShellApplication {
        name = "aljd";
        runtimeInputs = builtins.attrValues {
          inherit (pkgs) alejandra fd cargo;
          inherit (rust-overlay.packages.${system}.rust-nightly.availableComponents) rustfmt;
        };
        text = ''
          fd "$@" -t f -e nix -X alejandra -q '{}'
          cargo fmt --all
        '';
      });

    packages = forEachSystem (system: pkgs: {
      default = pkgs.callPackage ./package.nix {};
    });

    devShells = forEachSystem (system: pkgs: {
      default = pkgs.mkShell {
        inputsFrom = builtins.attrValues self.packages.${system};
        packages = builtins.attrValues {
          inherit (pkgs) rustc cargo clippy;
          inherit (rust-overlay.packages.${system}.rust-nightly.availableComponents) rustfmt;
        };
      };
    });
  };
}
