# kajiba-git (0054), built from the workspace's shared dependency artifacts.
{
  craneLib,
  commonCargoArgs,
  cargoArtifacts,
}:
craneLib.buildPackage (
  commonCargoArgs
  // {
    inherit cargoArtifacts;
    pname = "kajiba-git";
    cargoExtraArgs = "--locked -p kajiba-git";
    meta.mainProgram = "kajiba-git";
  }
)
