# kajiba-cli (0007), built by crane with the shared tonic client from kajiba-proto.
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
    pname = "kajiba-cli";
    cargoExtraArgs = "--locked -p kajiba-cli";
    # Tests build Git fixtures for worktree runs (0097).
    nativeCheckInputs = [ git ];
  }
)
