{
  lib,
  stdenv,
  rustPlatform,
}:
rustPlatform.buildRustPackage (finalAttrs: let
  cargoToml = lib.importTOML "${finalAttrs.src}/Cargo.toml";
in {
  inherit (cargoToml.package) name version;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./resources
      ./src
      ./build.rs
      ./Cargo.toml
      ./Cargo.lock
      ./Makefile
    ];
  };

  cargoLock = {
    allowBuiltinFetchGit = true;
    lockFile = "${finalAttrs.src}/Cargo.lock";
  };

  dontCargoInstall = true;

  makeFlags = [
    "prefix=${placeholder "out"}"
    "CARGO_TARGET_DIR=target/${stdenv.hostPlatform.rust.cargoShortTarget}"
  ];
})
