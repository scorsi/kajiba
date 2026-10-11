# kajiba-runner (0002), built by crane. kajiba-proto's build.rs needs protoc on PATH to turn
# proto/kajiba/v1/health.proto into Rust (0005, 0020).
{
  lib,
  pkgs,
  craneLib,
}:
craneLib.buildPackage {
  pname = "kajiba-runner";
  cargoExtraArgs = "-p kajiba-runner";
  strictDeps = true;
  nativeBuildInputs = [ pkgs.protobuf ];
  PROTOC = "${pkgs.protobuf}/bin/protoc";
  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources ../../crates)
      (craneLib.fileset.commonCargoSources ../../apps/server)
      (craneLib.fileset.commonCargoSources ../../apps/runner)
      ../../Cargo.toml
      ../../Cargo.lock
      ../../proto
    ];
  };
}
