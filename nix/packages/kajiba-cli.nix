# kajiba-cli (0007), the Go client generated from the same protos.
{ lib, buildGoModule }:
buildGoModule {
  pname = "kajiba-cli";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ../../apps/cli;
    fileset = lib.fileset.unions [
      ../../apps/cli/go.mod
      ../../apps/cli/go.sum
      ../../apps/cli/main.go
      ../../apps/cli/internal
    ];
  };
  vendorHash = "sha256-msdLEUSTRrxVH6zd9k475+ObsGw+z0vpzRw9W36yYJw=";

  # The module root has no cmd/ subdir, so buildGoModule names the binary after the
  # directory ("cli"); rename to match the command cobra actually registers ("kajiba").
  postInstall = ''
    mv $out/bin/cli $out/bin/kajiba
  '';
}
