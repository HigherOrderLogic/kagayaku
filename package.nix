{
  lib,
  stdenv,
  rustPlatform,
  libxkbcommon,
  libGL,
  wayland,
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

  buildInputs = [libxkbcommon libGL wayland];

  env.RUSTFLAGS = lib.concatMapStringsSep " " (s: "-C link-arg=${s}") [
    "-Wl,-rpath,${lib.makeLibraryPath [libxkbcommon libGL wayland]},--no-as-needed"
    "-lwayland-client"
    "-lxkbcommon"
    "-lGL"
  ];

  makeFlags = [
    "prefix=${placeholder "out"}"
    "CARGO_TARGET_DIR=target/${stdenv.hostPlatform.rust.cargoShortTarget}"
  ];
})
