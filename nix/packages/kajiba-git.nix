# kajiba-git (0054), built from the workspace's shared dependency artifacts.
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
    pname = "kajiba-git";
    cargoExtraArgs = "--locked -p kajiba-git";
    nativeCheckInputs = [ git ];
    meta.mainProgram = "kajiba-git";
  }
)
