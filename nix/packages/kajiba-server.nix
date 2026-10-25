# kajiba-server (0002), built by crane. kajiba-proto's build.rs needs protoc on PATH to turn
# proto/kajiba/v1/health.proto into Rust (0005, 0020).
{
  craneLib,
  commonCargoArgs,
  cargoArtifacts,
  git,
  procps,
}:
craneLib.buildPackage (
  commonCargoArgs
  // {
    inherit cargoArtifacts;
    pname = "kajiba-server";
    cargoExtraArgs = "--locked -p kajiba-server";
    # Tests create Git repositories and inspect descendant cleanup with ps.
    nativeCheckInputs = [
      git
      procps
    ];
    meta.mainProgram = "kajiba-server";
  }
)
