{
  lib,
  rustPlatform,
}:
let
  cargo = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "claude-code-sync";
  inherit (cargo.package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      (lib.fileset.maybeMissing ../tests)
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  doCheck = true;

  meta = {
    description = cargo.package.description;
    # The fork, which ships this flake; Cargo.toml still names upstream.
    homepage = "https://github.com/jasonpeacock/claude-code-sync";
    license = lib.licenses.mit;
    mainProgram = "claude-code-sync";
    platforms = lib.platforms.unix;
  };
}
