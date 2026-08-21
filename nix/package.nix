{
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "beckon";
  version = "0.1.0";
  src = ../.;
  cargoLock.lockFile = ../Cargo.lock;

  meta.mainProgram = "beckon";
}
