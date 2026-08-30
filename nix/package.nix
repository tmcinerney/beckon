{
  lib,
  makeWrapper,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "beckon";
  version = "0.2.1";
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
    description = "Glove80 state display and Herdr pane navigation";
    license = lib.licenses.mit;
    mainProgram = "beckon";
  };
}
