# kajiba-ui (0006, 0146): the static SvelteKit SPA bundle, shipped as its own artifact and
# served by kajiba-server with `--ui-dir` (the NixOS module passes it). Part of the pnpm
# workspace (pnpm-workspace.yaml at repo root), hence the repo-root src and the
# `--filter` build rather than a self-contained apps/ui source tree.
{
  lib,
  root ? ../..,
  stdenvNoCC,
  nodejs,
  pnpm,
  fetchPnpmDeps,
  pnpmConfigHook,
}:
stdenvNoCC.mkDerivation (finalAttrs: {
  pname = "kajiba-ui";
  version = lib.trim (builtins.readFile (root + "/VERSION"));

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/pnpm-workspace.yaml")
      (root + "/pnpm-lock.yaml")
      (root + "/apps/ui/package.json")
      (root + "/apps/ui/vite.config.ts")
      (root + "/apps/ui/dev-proxy.js")
      (root + "/apps/ui/tsconfig.json")
      (root + "/apps/ui/src")
      (root + "/apps/ui/tests")
      (root + "/apps/ui/static")
    ];
  };

  pnpmDeps = fetchPnpmDeps {
    inherit (finalAttrs) pname version src;
    inherit pnpm;
    fetcherVersion = 4;
    hash = "sha256-kdSCpUtISvy5EDTrjShNmZOdKQmjIfqL/iLhgQkCqtw=";
  };

  nativeBuildInputs = [
    nodejs
    pnpm
    pnpmConfigHook
  ];

  buildPhase = ''
    runHook preBuild
    pnpm --filter kajiba-ui build
    runHook postBuild
  '';

  doCheck = true;
  checkPhase = ''
    runHook preCheck
    pnpm --filter kajiba-ui check
    (cd apps/ui && node --test tests/*.test.js)
    runHook postCheck
  '';

  installPhase = ''
    runHook preInstall
    cp -r apps/ui/build $out
    runHook postInstall
  '';
})
