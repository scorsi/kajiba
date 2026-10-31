# J6 task 27: `kajiba ci run --worktree` from a client clone, end to end behind the nginx TLS
# front of `nixos-ci`: uncommitted changes run on the runner without a push, selected checks,
# routing by `requiredSystemFeatures`, the refusals and the worktree retention (every
# acceptance criterion of J6 marked worktree). Runner mTLS task 8: runners by SVID through the
# SNI relay of `nixos-ci`, and a runner whose declaration is removed refused as disabled (0124).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  kajibaRunnerPackage,
  kajibaCliPackage,
  testLib,
}:
let
  inherit (pkgs) lib;
  inherit (pkgs.stdenv.hostPlatform) system;
  arch = pkgs.stdenv.hostPlatform.parsed.cpu.name;
  # Public fixture credentials, never deployment secrets.
  writerPat = "kjb_" + lib.concatStrings (lib.replicate 43 "W");
  noScopePat = "kjb_" + lib.concatStrings (lib.replicate 43 "N");
  readerPat = "kjb_" + lib.concatStrings (lib.replicate 43 "R");
  svid = import ./svid.nix { inherit pkgs; };
  # The HTTP front's own test CA and certificate (SAN `forge`), for the CLI and git.
  tls = pkgs.runCommand "kajiba-test-tls" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir "$out"
    cd "$out"
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
      -subj "/CN=kajiba test CA" -keyout ca.key -out ca.pem
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
      -subj "/CN=forge" -keyout forge.key -out forge.csr
    printf '%s\n' subjectAltName=DNS:forge basicConstraints=CA:FALSE \
      extendedKeyUsage=serverAuth keyUsage=digitalSignature > forge.ext
    openssl x509 -req -in forge.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 3650 \
      -extfile forge.ext -out forge.pem
  '';
  build-1 = {
    spiffeId = svid.id "runner" "kajiba-runner";
    capabilities = {
      inherit arch;
      os = "linux";
      tags = [ "kvm" ];
      maxJobs = 2;
    };
  };
  # Fixture flake without inputs: plain derivations built by /bin/sh in the sandbox. `salt`
  # changes before every run so its checks build afresh; `pass` fails unless `state` is `ok`;
  # `vm` needs the `kvm` system feature. A second system exists only to be evaluated.
  flake = pkgs.writeText "flake.nix" ''
    {
      outputs = _:
        let
          salt = builtins.readFile ./salt;
          state = builtins.readFile ./state;
          checksFor = system:
            let
              check = name: script: extra: derivation ({
                inherit name system salt;
                builder = "/bin/sh";
                args = [ "-c" script ];
              } // extra);
            in
            {
              pass = check "pass" ("echo pass-" + state + "; test " + state + " = ok && echo > $out") { };
              a = check "a" "echo a-ok; echo > $out" { };
              b = check "b" "echo b-ok; echo > $out" { };
              vm = check "vm" "echo vm-ok; echo > $out" { requiredSystemFeatures = [ "kvm" ]; };
            };
        in
        {
          checks."${system}" = checksFor "${system}";
          checks.x86_64-darwin = checksFor "x86_64-darwin";
        };
    }
  '';
  lock = pkgs.writeText "flake.lock" ''{"nodes":{"root":{}},"root":"root","version":7}'';
  nixSettings = {
    experimental-features = [
      "nix-command"
      "flakes"
    ];
    # No network: never wait on a binary cache.
    substituters = lib.mkForce [ ];
  };
in
pkgs.testers.runNixOSTest {
  name = "kajiba-ci-worktree";

  nodes.forge = {
    imports = [
      ../nixosModules/server.nix
      (svid.install "kajiba-server" "forge")
    ];
    virtualisation.memorySize = 2048;
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      ui.enable = false;
      http.publicUrl = "https://forge";
      metrics.listen = "127.0.0.1:9431";
      users = {
        alice = { };
        writer = {
          sshKeys = [ (builtins.readFile ./fixtures/ssh-writer-key.pub) ];
          signingKeys = [ (builtins.readFile ./fixtures/ssh-signing-key.pub) ];
          tokens.ci = {
            sha256 = builtins.hashString "sha256" writerPat;
            scopes = [
              "repo:read"
              "ci:trigger"
            ];
          };
          tokens.read = {
            sha256 = builtins.hashString "sha256" noScopePat;
            scopes = [ "repo:read" ];
          };
        };
        reader.tokens.ci = {
          sha256 = builtins.hashString "sha256" readerPat;
          scopes = [
            "repo:read"
            "ci:trigger"
          ];
        };
      };
      runnerListener = {
        listen = "127.0.0.1:8443";
        trustDomain = "example.org";
      }
      // svid.files "kajiba-server";
      runners = {
        inherit build-1;
        # Declared without `kvm`; connected only by the routing subtest.
        build-2 = {
          spiffeId = svid.id "runner-2" "kajiba-runner";
          capabilities = {
            inherit arch;
            os = "linux";
          };
        };
      };
      repos.demo = {
        owner = "alice";
        description = "CI fixture";
        ciEnabled = true;
        permissions = {
          writer = "write";
          reader = "read";
        };
      };
      # Short enough for `admin retention` to purge the worktree runs within the test.
      ci.worktreeRetention = "1m";
    };
    # build-2's declaration removed: its row stays, disabled (0026).
    specialisation.retired.configuration.services.kajiba-server.runners = lib.mkForce {
      inherit build-1;
    };
    # The reverse proxy as in nixos-ci: `runners.forge` relayed as raw TCP by SNI to the runner
    # listener, any other name to the TLS front: gRPC (HTTP/2) to the server, streams kept
    # open, request bodies (bundle uploads) unbounded.
    services.nginx = {
      enable = true;
      streamConfig = ''
        map $ssl_preread_server_name $kajiba_upstream {
          runners.forge 127.0.0.1:8443;
          default 127.0.0.1:4443;
        }
        server {
          listen 443;
          listen [::]:443;
          ssl_preread on;
          proxy_pass $kajiba_upstream;
          proxy_timeout 1h;
        }
      '';
      virtualHosts.forge = {
        listen = [
          {
            addr = "127.0.0.1";
            port = 4443;
            ssl = true;
          }
        ];
        onlySSL = true;
        http2 = true;
        sslCertificate = "${tls}/forge.pem";
        sslCertificateKey = "${tls}/forge.key";
        extraConfig = ''
          client_max_body_size 0;
          client_body_timeout 1h;
          grpc_read_timeout 1h;
          grpc_send_timeout 1h;
        '';
        locations."/kajiba.".extraConfig = "grpc_pass grpc://127.0.0.1:8080;";
        locations."/".proxyPass = "http://127.0.0.1:8080";
      };
    };
    networking.firewall.allowedTCPPorts = [ 443 ];
    security.pki.certificateFiles = [ "${tls}/ca.pem" ];
    environment.systemPackages = [
      pkgs.git
      pkgs.openssh
      pkgs.sqlite
    ];
  };

  nodes.runner =
    { nodes, ... }:
    {
      imports = [
        ../nixosModules/runner.nix
        (svid.install "kajiba-runner" "runner")
      ];
      virtualisation.memorySize = 2048;
      services.kajiba-runner = {
        enable = true;
        package = kajibaRunnerPackage;
        name = "build-1";
        url = "https://runners.forge";
        tls = {
          trustDomain = "example.org";
          inherit (svid) serverId;
        }
        // svid.files "kajiba-runner";
        tags = [ "kvm" ];
        maxJobs = 2;
      };
      networking.hosts.${nodes.forge.networking.primaryIPAddress} = [ "runners.forge" ];
      nix.settings = nixSettings // {
        system-features = [
          "nixos-test"
          "benchmark"
          "big-parallel"
          "kvm"
        ];
      };
      environment.systemPackages = [
        kajibaRunnerPackage
        pkgs.git
      ];
    };

  nodes.client = {
    virtualisation.memorySize = 2048;
    virtualisation.diskSize = 2048;
    security.pki.certificateFiles = [ "${tls}/ca.pem" ];
    nix.settings = nixSettings;
    environment.systemPackages = [
      kajibaCliPackage
      pkgs.git
    ];
  };

  testScript = testLib + ''
    import json

    db = "/var/lib/kajiba-server/db/kajiba.sqlite"
    bundles_dir = "/var/lib/kajiba-server/ci/bundles"
    clone_dir = "/root/clone"
    writer_cli = f"cd {clone_dir} && kajiba --token-file /root/writer-token"
    no_scope_cli = f"cd {clone_dir} && kajiba --token-file /root/no-scope-token"
    reader_cli = f"cd {clone_dir} && kajiba --token-file /root/reader-token"
    ssh_command = "ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/known_hosts -o ConnectTimeout=5 -i /root/writer-key -p 2222"
    manual = "/var/lib/kajiba-runner-manual"
    build_2 = f'name = "build-2"\nurl = "https://runners.forge"\nstate_dir = "{manual}"\n[tls]\ntrust_domain = "example.org"\nserver_id = "${svid.serverId}"\nsvid_file = "{manual}/svid.pem"\nkey_file = "{manual}/key.pem"\nbundle_file = "{manual}/bundle.pem"\n'


    def sql(query):
        return forge.succeed(f"sqlite3 {db} {shlex.quote(query)}").strip()


    def as_git(command):
        return forge.succeed(f"runuser -u kajiba-git -- env HOME=/var/empty GIT_CONFIG_GLOBAL=/dev/null git --git-dir={repo_dir} {command}")


    def finished(run_id):
        query = f"SELECT state FROM run WHERE public_id = '{run_id}'"
        forge.wait_until_succeeds(f"sqlite3 {db} {shlex.quote(query)} | grep -Ex 'succeeded|failed|cancelled'")
        return sql(query)


    def salted(text):
        # Every worktree run builds afresh: the tracked `salt` differs from the last one.
        client.succeed(f"printf %s {shlex.quote(text)} > {clone_dir}/salt")


    def worktree_run(args, cli=None):
        # Exit code, stdout, stderr and the run id (from `kajiba: run <id>` on stderr).
        code, out = client.execute(f"{cli or writer_cli} ci run --worktree {args} 2>/tmp/err")
        err = client.succeed("cat /tmp/err")
        ids = [line.split()[2] for line in err.splitlines() if line.startswith("kajiba: run ")]
        return code, out, err, (ids[0] if ids else None)


    def clone_state():
        return client.succeed(f"git -C {clone_dir} for-each-ref; git -C {clone_dir} stash list; git -C {clone_dir} status --porcelain")


    start_all()
    forge.wait_for_unit("kajiba-server.service")
    forge.wait_for_unit("kajiba-sshd.service")
    forge.wait_for_unit("nginx.service")
    runner.wait_for_unit("kajiba-runner.service")
    repo_dir = "/var/lib/kajiba-git/repos/" + sql("SELECT public_id FROM repo WHERE config_key = 'demo'") + ".git"

    with subtest("main holds the fixture flake and its push run succeeds"):
        forge.succeed(
            "install -m 600 ${./fixtures/ssh-writer-key} /root/writer-key",
            "install -m 600 ${./fixtures/ssh-signing-key} /root/signing-key",
            "ssh-keyscan -p 2222 127.0.0.1 > /root/known_hosts",
            "git init -q -b main /root/work",
            "git -C /root/work config user.name Writer",
            "git -C /root/work config user.email writer@example.org",
            "git -C /root/work config gpg.format ssh",
            "git -C /root/work config user.signingkey /root/signing-key",
            "cp ${flake} /root/work/flake.nix",
            "cp ${lock} /root/work/flake.lock",
            "printf main > /root/work/salt",
            "printf ok > /root/work/state",
            "git -C /root/work add -A",
            "git -C /root/work commit -q -S -m fixture",
            f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git -C /root/work push -q kajiba-git@127.0.0.1:alice/demo.git HEAD:refs/heads/main",
        )
        main_sha = forge.succeed("git -C /root/work rev-parse HEAD").strip()
        query = f"SELECT public_id FROM run WHERE sha = '{main_sha}' AND trigger = 'push'"
        forge.wait_until_succeeds(f"test -n \"$(sqlite3 {db} {shlex.quote(query)})\"")
        assert finished(sql(query)) == "succeeded"
        client.succeed(
            f"git clone -q https://forge/alice/demo.git {clone_dir}",
            f"git -C {clone_dir} config user.name Writer",
            f"git -C {clone_dir} config user.email writer@example.org",
            "umask 077; printf '%s\\n%s\\n' https://forge '${writerPat}' > /root/writer-token",
            "umask 077; printf '%s\\n%s\\n' https://forge '${noScopePat}' > /root/no-scope-token",
            "umask 077; printf '%s\\n%s\\n' https://forge '${readerPat}' > /root/reader-token",
        )
        forge_refs = as_git("for-each-ref")

    with subtest("uncommitted changes run on the runner without a push, logs streamed, exit 0"):
        salted("first")
        client.succeed(f"printf staged > {clone_dir}/staged && git -C {clone_dir} add staged")
        client.succeed(f"printf untracked > {clone_dir}/untracked")
        before = clone_state()
        code, out, err, first = worktree_run("")
        assert code == 0 and "pass-ok" in out and "vm-ok" in out, (code, out, err)
        assert finished(first) == "succeeded"
        row = sql(f"SELECT trigger, ref IS NULL, checks IS NULL FROM run WHERE public_id = '{first}'")
        assert row == "worktree|1|1", row
        captured = sql(f"SELECT sha FROM run WHERE public_id = '{first}'")
        # Nothing new on the forge or in the clone; the captured commit is not served.
        assert clone_state() == before, (before, clone_state())
        assert as_git("for-each-ref") == forge_refs
        forge.fail(f"runuser -u kajiba-git -- env HOME=/var/empty GIT_CONFIG_GLOBAL=/dev/null git --git-dir={repo_dir} cat-file -e {captured}")
        # From an empty repository: the clone already has the commit, so git would not ask.
        client.succeed("git init -q /root/empty", "git -C /root/empty fetch -q https://forge/alice/demo.git main")
        client.fail(f"git -C /root/empty fetch -q https://forge/alice/demo.git {captured}")
        assert client.succeed(f"git -C {clone_dir} rev-parse HEAD").strip() != captured

    with subtest("a change that breaks a check exits non-zero with its logs"):
        salted("second")
        client.succeed(f"printf broken > {clone_dir}/state")
        code, out, err, broken = worktree_run("")
        assert code == 1 and "pass-broken" in out, (code, out, err)
        assert f"kajiba: run {broken} failed" in err, err
        assert finished(broken) == "failed"
        client.succeed(f"git -C {clone_dir} checkout -q -- state")

    with subtest("--check a builds only checks.<system>.a"):
        salted("third")
        code, out, err, selected = worktree_run("--check a")
        assert code == 0 and "a-ok" in out, (code, out, err)
        assert "b-ok" not in out and "pass-" not in out and "vm-ok" not in out, out
        assert sql(f"SELECT checks FROM run WHERE public_id = '{selected}'") == '["a"]'

    with subtest("a check requiring kvm runs only on a runner tagged kvm"):
        salted("fourth")
        runner.succeed("systemctl stop kajiba-runner.service")
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\")\" = 0")
        runner.succeed(
            f"install -d -o kajiba-runner -g kajiba-runner -m 0700 {manual}",
            f"install -o kajiba-runner -g kajiba-runner -m 0600 ${svid.ca}/runner-2/svid.pem ${svid.ca}/runner-2/key.pem ${svid.ca}/bundle.pem {manual}/",
            f"printf %s {shlex.quote(build_2)} > {manual}/runner.toml",
            # set +e: the subshell inherits errexit and timeout ends with 124.
            f"(set +e; runuser -u kajiba-runner -- env PATH=/run/current-system/sw/bin HOME={manual} timeout 120 kajiba-runner --config {manual}/runner.toml >{manual}/out 2>&1) >/dev/null 2>&1 &",
        )
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-2'\")\" = 1")
        code, out, err, _ = worktree_run("--check vm --detach")
        assert code == 0, (code, out, err)
        routed = out.strip()
        requirements = json.loads(sql(f"SELECT job.requirements FROM job JOIN run ON run.id = job.run_id WHERE run.public_id = '{routed}'"))
        assert requirements["tags"] == ["kvm"], requirements
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT job.pending_reason FROM job JOIN run ON run.id = job.run_id WHERE run.public_id = '{routed}'\")\" = tags-unmatched")
        runner.succeed("systemctl start kajiba-runner.service")
        assert finished(routed) == "succeeded"
        names = sql(f"SELECT DISTINCT n.name FROM job_attempt a JOIN runner n ON n.id = a.runner_id JOIN job ON job.id = a.job_id JOIN run ON run.id = job.run_id WHERE run.public_id = '{routed}'")
        assert names == "build-1", names
        out = client.succeed(f"{writer_cli} ci logs {routed}")
        assert "vm-ok" in out, out

    with subtest("refusals: role, scope, system without runner, bundle above 64 MiB"):
        salted("fifth")
        code, out, err, _ = worktree_run("", cli=reader_cli)
        assert code == 1 and "ROLE_MISSING" in err, (code, err)
        code, out, err, _ = worktree_run("", cli=no_scope_cli)
        assert code == 1 and "SCOPE_MISSING" in err, (code, err)
        code, out, err, _ = worktree_run("--system x86_64-darwin")
        assert code == 1 and "NO_MATCHING_RUNNER" in err, (code, err)
        # Incompressible and tracked: the bundle passes 64 MiB.
        client.succeed(
            f"head -c 70000000 /dev/urandom > {clone_dir}/big",
            f"git -C {clone_dir} add big",
        )
        code, out, err, _ = worktree_run("--check a")
        assert code == 1 and "BUNDLE_TOO_LARGE" in err, (code, err)
        client.succeed(f"git -C {clone_dir} rm -q --cached big", f"rm {clone_dir}/big")
        forge.succeed("test -z \"$(ls -A /var/lib/kajiba-server/tmp)\"")
        assert sql("SELECT COUNT(*) FROM run WHERE trigger = 'worktree'") == "4"

    with subtest("finished worktree runs lose their bundle at once, the runs go after ci.worktree_retention"):
        # All four runs are terminal: their bundles are gone, runs and logs stay (ADR 0122).
        forge.wait_until_succeeds(f"test -z \"$(ls -A {bundles_dir})\"")
        assert sql("SELECT COUNT(*) FROM run WHERE trigger = 'worktree' AND state IN ('queued', 'running')") == "0"
        forge.wait_until_succeeds(f"kajiba-server admin retention && test \"$(sqlite3 {db} \"SELECT COUNT(*) FROM run WHERE trigger = 'worktree'\")\" = 0", timeout=180)
        forge.succeed(f"test -z \"$(ls -A {bundles_dir})\"")
        assert sql("SELECT COUNT(*) FROM run WHERE trigger = 'push'") == "1"

    with subtest("a runner whose declaration is removed is refused as disabled, by its own SVID"):
        runner.execute(f"pkill -u kajiba-runner -f 'kajiba-runner --config {manual}/runner.toml'")
        forge.succeed("/run/current-system/specialisation/retired/bin/switch-to-configuration test")
        forge.wait_for_unit("kajiba-server.service")
        assert sql("SELECT disabled_at IS NOT NULL FROM runner WHERE name = 'build-2'") == "1"
        code, out = runner.execute(f"runuser -u kajiba-runner -- env PATH=/run/current-system/sw/bin HOME={manual} timeout 8 kajiba-runner --config {manual}/runner.toml 2>&1")
        assert code == 124 and "Unauthenticated" in out, out
        forge.succeed("journalctl --sync")
        refused = [json.loads(line) for line in forge.succeed("journalctl _SYSTEMD_UNIT=kajiba-server.service -o cat --no-pager").splitlines()]
        assert any(line.get("event") == "runner-refused" and line.get("reason") == "disabled" and line.get("runner") == "build-2" for line in refused), refused
        # build-1, still declared, is connected again after the restart.
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\")\" = 1")

    validate_logs(forge, "kajiba-server.service")
    validate_logs(runner, "kajiba-runner.service")
  '';
}
