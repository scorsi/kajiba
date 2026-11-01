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
  git,
  nodejs,
  pnpm,
  pnpmConfigHook,
  playwright-driver,
  kajibaServer,
  kajibaGit,
  kajibaRunner,
  kajibaCli,
  kajibaUi,
  # The test CA of the runner channel (nix/tests/svid.nix): forge/ and runner/ SVIDs, bundle.pem.
  svidCa,
}:
let
  # The fake-Nix harness runs without a system manager or the privileged job broker (0156).
  runner = kajibaRunner.override { withSystemd = false; };
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
    git
    nodejs
    kajibaServer
    kajibaGit
    runner
    kajibaCli
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

    # The runner channel's identities: the test CA's SVIDs copied under 0600 keys (the runner
    # refuses a key readable by others).
    rport="''${KAJIBA_E2E_RUNNER_PORT:-18443}"
    pat="kjb_$(head -c 32 /dev/urandom | basenc --base64url | tr -d '=')"
    case "$(uname -m)" in arm64 | aarch64) arch=aarch64 ;; *) arch=x86_64 ;; esac
    case "$(uname -s)" in Darwin) os=darwin ;; *) os=linux ;; esac
    mkdir -p "$dir/svid/forge" "$dir/svid/runner"
    cp ${svidCa}/bundle.pem "$dir/svid/bundle.pem"
    for unit in forge runner; do
      cp ${svidCa}/$unit/svid.pem ${svidCa}/$unit/key.pem "$dir/svid/$unit/"
      chmod 600 "$dir/svid/$unit/svid.pem" "$dir/svid/$unit/key.pem"
    done

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
    ssh_keys = ["$(cat ${../tests/fixtures/ssh-admin-key.pub})"]
    signing_keys = ["$(cat ${../tests/fixtures/ssh-signing-key.pub})"]
    [users.alice.tokens.ci]
    sha256 = "$(printf %s "$pat" | sha256sum | cut -d' ' -f1)"
    scopes = ["repo:read", "ci:trigger"]
    [users.reader]
    [runner_listener]
    listen = "127.0.0.1:$rport"
    trust_domain = "example.org"
    svid_file = "$dir/svid/forge/svid.pem"
    key_file = "$dir/svid/forge/key.pem"
    bundle_file = "$dir/svid/bundle.pem"
    [runners.e2e]
    spiffe_id = "spiffe://example.org/runner/kajiba-runner"
    capabilities = { arch = "$arch", os = "$os" }
    [repos.demo]
    owner = "alice"
    description = "Demo repository"
    ci = "manual"
    permissions.reader = "read"
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
      echo "--- kajiba-runner" >&2
      tail -n 50 "$dir/runner.log" >&2 || true
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

    # The declared repository's content: two branches, an annotated tag, a README and a nested
    # file, built with plain git (a push needs SSH and signed commits) and imported (0026).
    fixture="$dir/fixture"
    export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
    export GIT_AUTHOR_NAME=Alice GIT_AUTHOR_EMAIL=alice@example.org
    export GIT_COMMITTER_NAME=Alice GIT_COMMITTER_EMAIL=alice@example.org
    git init --quiet --initial-branch=main "$fixture"
    mkdir -p "$fixture/docs/guide"
    printf '# Demo\n\nA fixture repository.\n' > "$fixture/README.md"
    printf 'Start here.\n' > "$fixture/docs/guide/intro.md"
    git -C "$fixture" add .
    git -C "$fixture" commit --quiet -m "Add the README and a guide"
    git -C "$fixture" tag -a v1 -m "First release"
    git -C "$fixture" switch --quiet -c feature
    printf 'More.\n' >> "$fixture/docs/guide/intro.md"
    GIT_COMMITTER_NAME=Bob GIT_COMMITTER_EMAIL=bob@example.org \
      git -C "$fixture" commit --quiet -am "Extend the guide" -m "Explain the next steps for readers."
    git -C "$fixture" switch --quiet main
    # a tag named like a branch, on the commit main points at (duplicate-key regression)
    git -C "$fixture" tag feature
    git clone --quiet --bare "$fixture" "$dir/fixture.git"
    kajiba-server admin --socket "$dir/run/local.sock" import alice/demo "$dir/fixture.git" >/dev/null
    main_sha="$(git -C "$fixture" rev-parse refs/heads/main)"
    KAJIBA_E2E_FEATURE_SHA="$(git -C "$fixture" rev-parse refs/heads/feature)"
    KAJIBA_E2E_MAIN_SHA="$main_sha"
    export KAJIBA_E2E_FEATURE_SHA KAJIBA_E2E_MAIN_SHA

    # CI without a Nix sandbox: a fake `nix` first on the runner's PATH answers the host check
    # and `flake check` with scripted, timed output; the feature branch's tip fails.
    mkdir -p "$dir/bin"
    cat > "$dir/bin/nix" <<'EOF'
    #!/bin/sh
    if [ "$1 $2" = "config show" ]; then
      echo '{"sandbox":{"value":true},"accept-flake-config":{"value":false},"trusted-users":{"value":["root"]},"build-users-group":{"value":""}}'
      exit 0
    fi
    for ref; do :; done
    echo "evaluating flake ''${ref%%\?*}"
    sleep 1
    echo "building 'fmt'"
    sleep 1
    if [ "''${ref##*rev=}" = "$KAJIBA_E2E_FEATURE_SHA" ] || [ -e "$KAJIBA_E2E_FAIL_FLAG" ]; then
      echo "error: build of '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-fmt.drv' failed"
      exit 1
    fi
    # the other checks take their time, so that the log can be watched while it grows
    echo "building 'clippy'"
    sleep 2
    echo "building 'tests'"
    sleep 2
    echo "all checks passed"
    EOF
    chmod +x "$dir/bin/nix"
    printf 'http://localhost:%s\n%s\n' "$port" "$pat" > "$dir/token"
    export KAJIBA_E2E_TOKEN_FILE="$dir/token"

    cat > "$dir/runner.toml" <<EOF
    name = "e2e"
    url = "https://127.0.0.1:$rport"
    hostname = "e2e-host"
    backends = ["nix"]
    max_jobs = 1
    state_dir = "$dir/runner"
    [tls]
    trust_domain = "example.org"
    server_id = "spiffe://example.org/forge/kajiba-server"
    svid_file = "$dir/svid/runner/svid.pem"
    key_file = "$dir/svid/runner/key.pem"
    bundle_file = "$dir/svid/bundle.pem"
    EOF
    # A run queued while no runner is connected; the runner starts when the suite asks for it
    # (it creates $dir/start-runner), then takes that run first.
    KAJIBA_E2E_PENDING_RUN="$(kajiba --token-file "$dir/token" --development ci run --detach alice/demo "$main_sha")"
    export KAJIBA_E2E_PENDING_RUN
    # while this file exists every run fails (the activity scenario fails one on main)
    export KAJIBA_E2E_FAIL_FLAG="$dir/fail-runs"
    export KAJIBA_E2E_START_RUNNER="$dir/start-runner"
    (
      while [ ! -e "$dir/start-runner" ]; do sleep 0.2; done
      PATH="$dir/bin:$PATH" exec kajiba-runner --config "$dir/runner.toml" 2> "$dir/runner.log"
    ) &
    pids+=("$!")

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
