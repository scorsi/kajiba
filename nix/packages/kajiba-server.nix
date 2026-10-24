# kajiba-server (0002), built by crane. kajiba-proto's build.rs needs protoc on PATH to turn
# proto/kajiba/v1/health.proto into Rust (0005, 0020).
{
  craneLib,
  commonCargoArgs,
  cargoArtifacts,
}:
craneLib.buildPackage (
  commonCargoArgs
  // {
    inherit cargoArtifacts;
    pname = "kajiba-server";
    cargoExtraArgs = "--locked -p kajiba-server";
  }
)
