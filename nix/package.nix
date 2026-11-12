{
  lib,
  makeWrapper,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "beckon";
  version = "0.4.0";
  src = ../.;
  cargoLock.lockFile = ../Cargo.lock;
  nativeBuildInputs = [ makeWrapper ];

  # `beckond` is intentionally a wrapper rather than a second Rust binary: the
  # CLI and daemon share one versioned implementation, while the conventional
  # daemon name remains directly invokable from a user's PATH.
  postInstall = ''
    makeWrapper "$out/bin/beckon" "$out/bin/beckond" --add-flags "daemon"
  '';

  meta = {
    description = "Agent navigation and pluggable status displays";
    license = lib.licenses.mit;
    mainProgram = "beckon";
  };
}
