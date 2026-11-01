# Keep workspace dependency artifacts shared, but each binary's real sources private.
{
  pkgs,
  lib,
  craneLib,
  root ? ../..,
}:
let
  members = [
    "crates/kajiba-core"
    "crates/kajiba-exec"
    "crates/kajiba-gitstore"
    "crates/kajiba-proto"
    "apps/server"
    "apps/git"
    "apps/runner"
    "apps/cli"
  ];
  manifests = map (member: root + "/${member}/Cargo.toml") members;
  cargoSource =
    selected:
    lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions (
        manifests
        ++ [
          (root + "/Cargo.toml")
          (root + "/Cargo.lock")
          (root + "/VERSION")
          (lib.fileset.fileFilter (file: file.hasExt "proto") (root + "/proto"))
        ]
        ++ lib.concatMap (
          member:
          let
            path = root + "/${member}";
          in
          [ (craneLib.fileset.commonCargoSources path) ]
          ++ lib.optional (builtins.pathExists (path + "/tests/fixtures")) (path + "/tests/fixtures")
          ++ lib.optional (builtins.pathExists (path + "/migrations")) (path + "/migrations")
        ) selected
      );
    };
  commonCargoArgs = {
    pname = "kajiba-workspace";
    version = lib.trim (builtins.readFile (root + "/VERSION"));
    src = cargoSource members;
    strictDeps = true;
    nativeBuildInputs = [
      pkgs.protobuf
      pkgs.pkg-config
    ];
    # webauthn-rs-core links OpenSSL (0076).
    buildInputs = [ pkgs.openssl ];
    PROTOC = "${pkgs.protobuf}/bin/protoc";
    cargoExtraArgs = "--locked --workspace";
  };
  cargoArtifacts = craneLib.buildDepsOnly commonCargoArgs;
  cargoArgsFor =
    selected:
    commonCargoArgs
    // {
      src = cargoSource selected;
      # Cargo parses every workspace member even with -p; keep their manifests and
      # stub only unselected targets so the shared lockfile remains valid.
      postPatch = lib.concatMapStringsSep "\n" (member: ''
        mkdir -p ${member}/src
        echo 'fn main() {}' > ${member}/src/${if lib.hasPrefix "apps/" member then "main" else "lib"}.rs
      '') (lib.subtractLists selected members);
    };
  shared = [
    "crates/kajiba-core"
    "crates/kajiba-proto"
  ];
in
{
  inherit commonCargoArgs cargoArtifacts;
  packages = {
    kajiba-server = pkgs.callPackage ./kajiba-server.nix {
      inherit craneLib cargoArtifacts;
      commonCargoArgs = cargoArgsFor (
        shared
        ++ [
          "apps/server"
          "crates/kajiba-gitstore"
        ]
      );
    };
    kajiba-git = pkgs.callPackage ./kajiba-git.nix {
      inherit craneLib cargoArtifacts;
      commonCargoArgs = cargoArgsFor (
        shared
        ++ [
          "apps/git"
          "crates/kajiba-gitstore"
        ]
      );
    };
    kajiba-runner = pkgs.callPackage ./kajiba-runner.nix {
      inherit craneLib cargoArtifacts;
      # gitstore is also needed by the runner's dev-dependencies.
      commonCargoArgs = cargoArgsFor (
        shared
        ++ [
          "apps/runner"
          "crates/kajiba-exec"
          "crates/kajiba-gitstore"
        ]
      );
    };
    kajiba-cli = pkgs.callPackage ./kajiba-cli.nix {
      inherit craneLib cargoArtifacts;
      commonCargoArgs = cargoArgsFor (shared ++ [ "apps/cli" ]);
    };
    kajiba-ui = pkgs.callPackage ./kajiba-ui.nix { inherit root; };
  };
}
