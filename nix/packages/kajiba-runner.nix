# kajiba-runner (0002), built by crane. kajiba-proto's build.rs needs protoc on PATH to turn
# proto/kajiba/v1/health.proto into Rust (0005, 0020).
{
  craneLib,
  commonCargoArgs,
  cargoArtifacts,
  git,
}:
craneLib.buildPackage (
  commonCargoArgs
  // {
    inherit cargoArtifacts;
    pname = "kajiba-runner";
    cargoExtraArgs = "--locked -p kajiba-runner";
    # Tests build Git fixtures for the source cache (0106).
    nativeCheckInputs = [ git ];
    meta.mainProgram = "kajiba-runner";
  }
)
