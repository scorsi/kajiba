# `nix run .#e2e` (0015): both daemons from the flake's packages, unprivileged in a temporary
# directory (0093, 0095), the web application through --ui-dir (0094), and the Playwright
# suite of apps/ui with nixpkgs' browsers. Exits with the suite's status. Outside the Nix
# sandbox only (CI job `e2e`), never part of `nix flake check`.
{
  lib,
  stdenvNoCC,
  writeShellApplication,
  coreutils,
  curl,
  nodejs,
  pnpm,
  pnpmConfigHook,
  playwright-driver,
  kajibaServer,
  kajibaGit,
  kajibaUi,
}:
let
  # The suite with its node_modules (same lockfile and pnpm store as kajiba-ui).
  suite = stdenvNoCC.mkDerivation {
    pname = "kajiba-e2e-suite";
    inherit (kajibaUi) version pnpmDeps;
    src = lib.fileset.toSource {
      root = ../..;
      fileset = lib.fileset.unions [
        (lib.fileset.fromSource kajibaUi.src)
        ../../apps/ui/playwright.config.ts
        ../../apps/ui/e2e
      ];
    };
    nativeBuildInputs = [
      nodejs
      pnpm
      pnpmConfigHook
    ];
    dontBuild = true;
    installPhase = ''
      mkdir -p "$out/apps"
      cp -a node_modules "$out/"
      cp -a apps/ui "$out/apps/"
    '';
  };
in
writeShellApplication {
  name = "kajiba-e2e";
  runtimeInputs = [
    coreutils
    curl
    nodejs
    kajibaServer
    kajibaGit
  ];
  text = ''
    port="''${KAJIBA_E2E_PORT:-18080}"
    output="''${KAJIBA_E2E_OUTPUT:-$PWD/e2e-results}"
    # Short: socket paths must fit in sun_path.
    dir="$(mktemp -d /tmp/kajiba-e2e.XXXXXX)"
    pids=()
    # shellcheck disable=SC2329 # invoked by the trap
    cleanup() {
      for pid in "''${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
      wait
      rm -rf "$dir"
    }
    trap cleanup EXIT
    user="$(id -un)"
    mkdir -p "$dir/run" "$dir/run-git" "$dir/hooks"

    cat > "$dir/server.toml" <<EOF
    state_dir = "$dir/server"
    run_dir = "$dir/run"
    [http]
    listen = "127.0.0.1:$port"
    public_url = "http://localhost:$port"
    [git]
    state_dir = "$dir/git"
    socket = "$dir/run-git/local.sock"
    user = "$user"
    [auth]
    session_ttl = "1m"
    [users.alice]
    is_admin = true
    EOF
    cat > "$dir/git.toml" <<EOF
    state_dir = "$dir/git"
    server_socket = "$dir/run/git.sock"
    run_dir = "$dir/run-git"
    server_user = "$user"
    user = "$user"
    hooks_path = "$dir/hooks"
    EOF

    kajiba-git serve --config "$dir/git.toml" 2> "$dir/git.log" &
    pids+=("$!")
    # As the module orders the units: the server starts once the git side has prepared its
    # state and bound its socket.
    for _ in $(seq 60); do
      [ -S "$dir/run-git/local.sock" ] && break
      sleep 0.5
    done
    kajiba-server serve --config "$dir/server.toml" --ui-dir ${kajibaUi} 2> "$dir/server.log" &
    pids+=("$!")
    logs() {
      echo "--- kajiba-git" >&2
      tail -n 50 "$dir/git.log" >&2
      echo "--- kajiba-server" >&2
      tail -n 50 "$dir/server.log" >&2
    }
    ready=
    for _ in $(seq 60); do
      # A dead daemon fails at once, and another server on the port is never mistaken for ours.
      for pid in "''${pids[@]}"; do kill -0 "$pid" 2>/dev/null || break 2; done
      if curl --silent --fail --output /dev/null "http://127.0.0.1:$port/readyz"; then
        ready=1
        break
      fi
      sleep 1
    done
    if [ -z "$ready" ]; then
      echo "kajiba-e2e: the daemons did not become ready" >&2
      logs
      exit 1
    fi

    export KAJIBA_E2E_URL="http://localhost:$port"
    export KAJIBA_E2E_ADMIN_SOCKET="$dir/run/local.sock"
    export KAJIBA_E2E_OUTPUT="$output"
    export PLAYWRIGHT_BROWSERS_PATH=${playwright-driver.browsers}
    export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true
    export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
    status=0
    (cd ${suite}/apps/ui && ./node_modules/.bin/playwright test "$@") || status=$?
    if [ "$status" -ne 0 ]; then logs; fi
    exit "$status"
  '';
}
