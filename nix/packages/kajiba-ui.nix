# kajiba-ui (0006): the static SvelteKit SPA bundle. Embedded into kajiba-server
# starting J5; for now it's built and shipped as its own artifact. Part of the pnpm
# workspace (pnpm-workspace.yaml at repo root), hence the repo-root src and the
# `--filter` build rather than a self-contained apps/ui source tree.
{
  lib,
  stdenvNoCC,
  nodejs,
  pnpm,
  fetchPnpmDeps,
  pnpmConfigHook,
}:
stdenvNoCC.mkDerivation (finalAttrs: {
  pname = "kajiba-ui";
  version = lib.trim (builtins.readFile ../../VERSION);

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../pnpm-workspace.yaml
      ../../pnpm-lock.yaml
      ../../apps/ui/package.json
      ../../apps/ui/vite.config.ts
      ../../apps/ui/tsconfig.json
      ../../apps/ui/src
      ../../apps/ui/tests
      ../../apps/ui/static
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
