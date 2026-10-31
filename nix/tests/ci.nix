# J6 task 20: a declared runner behind an nginx TLS front runs `nix flake check` on pushes and
# manual triggers; admission, leases, reconnection, pending reasons, the journal, the CLI and
# log retention, end to end (every non-worktree acceptance criterion of J6).
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
  # Public fixture credentials, never deployment secrets.
  writerPat = "kjb_" + lib.concatStrings (lib.replicate 43 "W");
  readerPat = "kjb_" + lib.concatStrings (lib.replicate 43 "R");
  secret = "runner-fixture-secret-not-a-secret";
  version = kajibaRunnerPackage.version;
  otherVersion = if builtins.elemAt (lib.splitString "." version) 1 == "2" then "0.3.0" else "0.2.0";
  # A test CA and the forge's certificate (SAN `forge`), built in the test (not development mode).
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
  secretRule = user: {
    "/run/secrets/kajiba-runner".f = {
      inherit user;
      group = user;
      mode = "0400";
      argument = secret;
    };
  };
in
pkgs.testers.runNixOSTest {
  name = "kajiba-ci";

  nodes.forge = {
    imports = [ ../nixosModules/server.nix ];
    virtualisation.memorySize = 2048;
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
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
        };
        reader.tokens.ci = {
          sha256 = builtins.hashString "sha256" readerPat;
          scopes = [
            "repo:read"
            "ci:trigger"
          ];
        };
      };
      runners.build-1 = {
        tokenFile = "/run/secrets/kajiba-runner";
        capabilities = {
          arch = pkgs.stdenv.hostPlatform.parsed.cpu.name;
          os = "linux";
          maxJobs = 2;
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
      # Short enough for `admin retention` to delete the first runs' logs within the test.
      ci.logRetention = "1m";
    };
    systemd.tmpfiles.settings."10-kajiba-secret" = secretRule "kajiba-server";
    # TLS front: gRPC (HTTP/2) to the server, long-lived streams kept open, request bodies
    # (the runner's log stream) unbounded; one access-log line per request with its size.
    services.nginx = {
      enable = true;
      commonHttpConfig = ''
        log_format kajiba '$remote_addr $request_uri $status $body_bytes_sent';
      '';
      virtualHosts.forge = {
        onlySSL = true;
        http2 = true;
        sslCertificate = "${tls}/forge.pem";
        sslCertificateKey = "${tls}/forge.key";
        extraConfig = ''
          access_log /var/log/nginx/kajiba.log kajiba;
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
      kajibaCliPackage
      pkgs.git
      pkgs.openssh
      pkgs.sqlite
      pkgs.curl
      pkgs.grpcurl
    ];
  };

  nodes.runner = {
    imports = [ ../nixosModules/runner.nix ];
    virtualisation.memorySize = 2048;
    services.kajiba-runner = {
      enable = true;
      package = kajibaRunnerPackage;
      name = "build-1";
      url = "https://forge";
      tokenFile = "/run/secrets/kajiba-runner";
      maxJobs = 2;
      metrics.listen = "127.0.0.1:9432";
    };
    systemd.tmpfiles.settings."10-kajiba-secret" = secretRule "kajiba-runner";
    security.pki.certificateFiles = [ "${tls}/ca.pem" ];
    nix.settings = {
      experimental-features = [
        "nix-command"
        "flakes"
      ];
      # No network: never wait on a binary cache.
      substituters = lib.mkForce [ ];
      # The fixture's closure: /bin/sh (busybox, Nix's default) and coreutils for `sleep`,
      # already in this system's store, so nothing is fetched.
      extra-sandbox-paths = [ "/tools=${pkgs.coreutils}/bin" ];
    };
    environment.systemPackages = [
      kajibaRunnerPackage
      pkgs.git
    ];
  };

  testScript = testLib + ''
    import base64
    import json
    import re
    import time

    db = "/var/lib/kajiba-server/db/kajiba.sqlite"
    logs_dir = "/var/lib/kajiba-server/ci/logs"
    metrics_url = "http://127.0.0.1:9431/metrics"
    writer_cli = "kajiba --token-file /root/writer-token"
    reader_cli = "kajiba --token-file /root/reader-token"
    ssh_command = "ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/known_hosts -o ConnectTimeout=5 -i /root/writer-key -p 2222"
    proto = "${../../proto}"
    manual = "/var/lib/kajiba-runner-manual"

    # Fixture flake without inputs: plain derivations built by /bin/sh in the sandbox. `salt`
    # changes with every commit so each run builds its checks afresh.
    FLAKE = """{
      outputs = _:
        let
          check = name: script: derivation {
            inherit name;
            system = "${system}";
            builder = "/bin/sh";
            args = [ "-c" script ];
            salt = builtins.readFile ./salt;
          };
        in
        {
          checks."${system}" = {
            pass = check "pass" "echo pass-ok; echo > $out";
    EXTRA      };
        };
    }
    """
    # More than 64 KiB of output: several log chunks, and enough to close a pipe early.
    FAIL = '        fail = check "fail" "i=0; while [ $i -lt 3000 ]; do i=$((i+1)); echo fail-line-$i-0123456789abcdef0123456789abcdef0123456789abcdef; done; exit 3";\n'
    LONG = '        long = check "long" "echo long-start; /tools/sleep 45; echo long-end; echo > $out";\n'
    LOCK = '{"nodes":{"root":{}},"root":"root","version":7}\n'


    def sql(query):
        return forge.succeed(f"sqlite3 {db} {shlex.quote(query)}").strip()


    def unit_lines(machine, unit):
        machine.succeed("journalctl --sync")
        out = machine.succeed(f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager")
        return [json.loads(line) for line in out.splitlines()]


    def metric(machine, url, series):
        lines = machine.succeed(f"curl -fsS {url}").splitlines()
        return float(next(line.split()[-1] for line in lines if line.startswith(series + " ")))


    def commit(extra, message, sign=True):
        text = FLAKE.replace("EXTRA", extra)
        forge.succeed(
            f"printf %s {shlex.quote(text)} > /root/work/flake.nix",
            f"printf %s {shlex.quote(LOCK)} > /root/work/flake.lock",
            f"printf %s {shlex.quote(message)} > /root/work/salt",
            "git -C /root/work add -A",
            f"git -C /root/work commit -q {'-S' if sign else '--no-gpg-sign'} -m {shlex.quote(message)}",
        )
        return forge.succeed("git -C /root/work rev-parse HEAD").strip()


    def push(sha):
        forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git -C /root/work push -q kajiba-git@127.0.0.1:alice/demo.git {sha}:refs/heads/main")
        query = f"SELECT public_id FROM run WHERE sha = '{sha}' AND trigger = 'push'"
        forge.wait_until_succeeds(f"test -n \"$(sqlite3 {db} {shlex.quote(query)})\"")
        return sql(query)


    def finished(run_id):
        query = f"SELECT state FROM run WHERE public_id = '{run_id}'"
        forge.wait_until_succeeds(f"sqlite3 {db} {shlex.quote(query)} | grep -Ex 'succeeded|failed|cancelled'")
        return sql(query)


    def attempts(run_id):
        # [(public id, number, state)] of the run's only job.
        out = sql(f"SELECT a.public_id, a.number, a.state FROM job_attempt a JOIN job ON job.id = a.job_id JOIN run ON run.id = job.run_id WHERE run.public_id = '{run_id}' ORDER BY a.number")
        return [tuple(line.split("|")) for line in out.splitlines()]


    def log_of(attempt_id):
        return forge.succeed(f"cat {logs_dir}/{attempt_id}.log")


    def started(run_id):
        # The current attempt's log shows the long check running.
        forge.wait_until_succeeds(f"grep -q long-start {logs_dir}/$(sqlite3 {db} \"SELECT a.public_id FROM job_attempt a JOIN job ON job.id = a.job_id JOIN run ON run.id = job.run_id WHERE run.public_id = '{run_id}' ORDER BY a.number DESC LIMIT 1\").log")


    def finishes(run_id):
        return int(sql(f"SELECT COUNT(*) FROM event WHERE kind = 'run-finished' AND payload LIKE '%{run_id}%'"))


    def watch_requests():
        return [line for line in unit_lines(forge, "kajiba-server.service") if line.get("event") == "request" and line.get("method") == "/kajiba.v1.RunService/WatchRun"]


    def has_reason(out, reason):
        # grpcurl prints an ErrorInfo it cannot resolve as base64 `@value`.
        values = re.findall(r'"@value": "([A-Za-z0-9+/=]+)"', out)
        return reason in out or any(reason.encode() in base64.b64decode(value) for value in values)


    def hello(token, hello_version, tags):
        message = {"hello": {"name": "build-1", "version": hello_version, "hostname": "grpcurl", "capabilities": {"arch": "${pkgs.stdenv.hostPlatform.parsed.cpu.name}", "os": "linux", "backends": ["nix"], "tags": tags, "maxJobs": 1}}}
        code, out = forge.execute(f"grpcurl -import-path {proto} -proto kajiba/runner/v1/runner.proto -H {shlex.quote('authorization: Bearer ' + token)} -d {shlex.quote(json.dumps(message))} forge:443 kajiba.runner.v1.RunnerService/Connect 2>&1")
        assert code != 0, out
        return out


    def manual_runner(config, seconds, env=""):
        # A second runner process on the same host, as the runner user, for `seconds`.
        runner.succeed(f"printf %s {shlex.quote(config)} > {manual}/runner.toml")
        return runner.execute(f"runuser -u kajiba-runner -- env PATH=/run/current-system/sw/bin HOME={manual} {env} timeout {seconds} kajiba-runner --config {manual}/runner.toml 2>&1")


    def runner_toml(extra):
        return f'name = "build-1"\nurl = "https://forge"\ntoken_file = "/run/secrets/kajiba-runner"\nstate_dir = "{manual}"\n{extra}'


    start_all()
    forge.wait_for_unit("kajiba-server.service")
    forge.wait_for_unit("kajiba-sshd.service")
    forge.wait_for_unit("nginx.service")
    runner.wait_for_unit("kajiba-runner.service")
    forge.succeed(
        "install -m 600 ${./fixtures/ssh-writer-key} /root/writer-key",
        "install -m 600 ${./fixtures/ssh-signing-key} /root/signing-key",
        "ssh-keyscan -p 2222 127.0.0.1 > /root/known_hosts",
        "umask 077; printf '%s\\n%s\\n' https://forge '${writerPat}' > /root/writer-token",
        "umask 077; printf '%s\\n%s\\n' https://forge '${readerPat}' > /root/reader-token",
        "git init -q -b main /root/work",
        "git -C /root/work config user.name Writer",
        "git -C /root/work config user.email writer@example.org",
        "git -C /root/work config gpg.format ssh",
        "git -C /root/work config user.signingkey /root/signing-key",
        # Incompressible: the first bundle is large, later ones only carry new commits.
        "head -c 600000 /dev/urandom > /root/work/blob",
        "mkdir -p -m 0755 /var/lib/kajiba-fixtures",
    )
    runner.succeed(f"install -d -o kajiba-runner -g kajiba-runner -m 0700 {manual}")
    repo_dir = "/var/lib/kajiba-git/repos/" + sql("SELECT public_id FROM repo WHERE config_key = 'demo'") + ".git"

    with subtest("the runner connects over HTTPS and shows its capabilities and OS hostname"):
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\")\" = 1")
        row = sql("SELECT version, hostname, capabilities FROM runner WHERE name = 'build-1'").split("|", 2)
        assert row[0] == "${version}" and row[1] == "runner", row
        capabilities = json.loads(row[2])
        assert capabilities["arch"] == "${pkgs.stdenv.hostPlatform.parsed.cpu.name}" and capabilities["os"] == "linux", capabilities
        assert capabilities["max_jobs"] == 2, capabilities
        assert metric(forge, metrics_url, "kajiba_runners_connected") == 1

    with subtest("wrong secret, extra capability and another 0.minor are refused"):
        out = hello("wrong-secret", "${version}", [])
        assert "Unauthenticated" in out and has_reason(out, "CREDENTIAL_INVALID"), out
        out = hello("${secret}", "${otherVersion}", [])
        assert "FailedPrecondition" in out and has_reason(out, "VERSION_INCOMPATIBLE"), out
        out = hello("${secret}", "${version}", ["kvm"])
        assert "InvalidArgument" in out and has_reason(out, "INVALID_FIELD"), out
        # The real runner, announcing a tag the server did not declare.
        code, out = manual_runner(runner_toml('tags = ["kvm"]\n'), 8)
        assert code == 124 and "InvalidArgument" in out, out
        refusals = [line.get("reason") for line in unit_lines(forge, "kajiba-server.service") if line.get("event") == "runner-refused"]
        assert {"secret", "version", "capabilities"} <= set(refusals), refusals
        for reason in ["secret", "version", "capabilities"]:
            assert metric(forge, metrics_url, f'kajiba_runner_refusals_total{{reason="{reason}"}}') >= 1
        # E11: an unauthenticated refusal is logged and counted, never journaled.
        journaled = [json.loads(p)["reason"] for p in sql("SELECT payload FROM event WHERE kind = 'runner-refused'").splitlines()]
        assert "secret" not in journaled and {"version", "capabilities"} <= set(journaled), journaled
        # The declared runner kept its connection.
        assert sql("SELECT connected FROM runner WHERE name = 'build-1'") == "1"

    with subtest("plain http without development mode and a host without the sandbox refuse to start"):
        plain = runner_toml("").replace("https://forge", "http://forge")
        runner.succeed(f"printf %s {shlex.quote(plain)} > {manual}/http.toml")
        out = runner.fail(f"kajiba-runner config check {manual}/http.toml 2>&1")
        assert "url" in out, out
        code, out = runner.execute(f"runuser -u kajiba-runner -- env PATH=/run/current-system/sw/bin HOME={manual} timeout 30 kajiba-runner --config {manual}/http.toml 2>&1")
        assert code == 1 and "config-invalid" in out and "url" in out, out
        code, out = manual_runner(runner_toml(""), 30, env="NIX_CONFIG='sandbox = false'")
        assert code == 1 and "sandbox must be true" in out, out

    with subtest("the hostname override is reported instead of the OS name"):
        runner.succeed("systemctl stop kajiba-runner.service")
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\")\" = 0")
        code, out = manual_runner(runner_toml('hostname = "vm-host.example.org"\nmax_jobs = 2\n'), 10)
        assert code == 124, out
        assert sql("SELECT hostname FROM runner WHERE name = 'build-1'") == "vm-host.example.org"
        runner.succeed("systemctl start kajiba-runner.service")
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT hostname FROM runner WHERE name = 'build-1' AND connected = 1\")\" = runner")

    with subtest("a signed push on main runs the checks and succeeds"):
        c1 = commit("", "first")
        r1 = push(c1)
        out = forge.succeed(f"{writer_cli} ci watch {r1} 2>/tmp/r1.err")
        assert "pass-ok" in out, out
        assert finished(r1) == "succeeded"
        assert "attempt 1 on build-1 (runner)" in forge.succeed("cat /tmp/r1.err")

    with subtest("a failing check fails the run and the CLI summarizes it on stderr"):
        c2 = commit(FAIL, "second")
        r2 = push(c2)
        code, out = forge.execute(f"{writer_cli} ci watch {r2} 2>/tmp/r2.err")
        err = forge.succeed("cat /tmp/r2.err")
        assert code == 1 and "fail-line-3000" in out, (code, err)
        assert f"kajiba: run {r2} failed: exit status 1" in err and "fail-line-3000" in err, err
        summary = err.split(f"run {r2} failed", 1)[1].splitlines()[1:]
        assert len(summary) == 20, summary
        assert finished(r2) == "failed"
        # Manual run of the same commit: `ci run` follows and exits 1.
        code, _ = forge.execute(f"{writer_cli} ci run alice/demo {c2} 2>/tmp/r2-manual.err")
        assert code == 1, forge.succeed("cat /tmp/r2-manual.err")

    with subtest("a closed pipe ends kajiba ci logs quietly"):
        assert forge.succeed(f"set -o pipefail; {writer_cli} ci logs {r2} 2>/tmp/head.err | head -c 4096").strip() != ""
        assert forge.succeed("cat /tmp/head.err") == ""

    with subtest("refs/contrib does not trigger; a write user can, a read user cannot"):
        forge.succeed("git -C /root/work checkout -q -b topic")
        c3 = commit("", "contribution", sign=False)
        forge.succeed(
            "git -C /root/work bundle create /var/lib/kajiba-fixtures/topic.bundle main..topic",
            "chmod 0644 /var/lib/kajiba-fixtures/topic.bundle",
            "git -C /root/work checkout -q main",
            f"runuser -u kajiba-git -- env HOME=/var/empty GIT_CONFIG_GLOBAL=/dev/null git --git-dir={repo_dir} fetch -q /var/lib/kajiba-fixtures/topic.bundle topic:refs/contrib/writer/topic",
        )
        time.sleep(3)
        assert sql(f"SELECT COUNT(*) FROM run WHERE sha = '{c3}'") == "0"
        code, out = forge.execute(f"{reader_cli} ci run alice/demo {c3} 2>&1")
        assert code == 1 and "ROLE_MISSING" in out, out
        out = forge.succeed(f"{writer_cli} ci run alice/demo {c3} 2>/dev/null")
        assert "pass-ok" in out, out
        assert sql(f"SELECT trigger, state FROM run WHERE sha = '{c3}'") == "manual|succeeded"

    with subtest("sources travel only as bundles, incremental after the first"):
        addresses = forge.succeed("getent ahosts runner | cut -d ' ' -f 1 | sort -u").split()
        requests = [line.split() for line in forge.succeed("cat /var/log/nginx/kajiba.log").splitlines()]
        from_runner = [r for r in requests if r[0] in addresses]
        assert from_runner and all(r[1].startswith("/kajiba.runner.v1.RunnerService/") for r in from_runner), from_runner
        bundles = [int(r[3]) for r in from_runner if r[1] == "/kajiba.runner.v1.RunnerService/FetchRunBundle"]
        assert len(bundles) >= 3 and bundles[0] > 600000, bundles
        assert all(size < 100000 for size in bundles[1:]), bundles
        methods = {line.get("method") for line in unit_lines(forge, "kajiba-server.service") if line.get("event") == "request"}
        assert not {"git-info-refs", "git-upload-pack"} & methods, methods

    with subtest("a WatchRun stream outlives the 30 s request timeout; status says 75 meanwhile"):
        c4 = commit(LONG, "fourth")
        r4 = push(c4)
        before = len(watch_requests())
        forge.succeed(f"(set +e; begin=$(date +%s); {writer_cli} ci watch {r4} >/tmp/r4.out 2>/tmp/r4.err; echo $? $(( $(date +%s) - begin )) >/tmp/r4.status) >/dev/null 2>&1 &")
        started(r4)
        code, _ = forge.execute(f"{writer_cli} ci status {r4} 2>/dev/null")
        assert code == 75, code
        forge.wait_until_succeeds("test -s /tmp/r4.status", timeout=300)
        status, seconds = forge.succeed("cat /tmp/r4.status").split()
        assert status == "0" and int(seconds) > 30, (status, seconds)
        out = forge.succeed("cat /tmp/r4.out")
        assert "long-start" in out and "long-end" in out, out
        new = watch_requests()[before:]
        assert len(new) == 1 and new[0].get("grpc_code") == "OK" and new[0]["dur_us"] > 30000000, new

    with subtest("a server restart during a job: the runner reconnects, nothing lost or doubled"):
        c5 = commit(LONG, "fifth")
        r5 = push(c5)
        started(r5)
        forge.succeed("systemctl restart kajiba-server.service")
        forge.wait_for_unit("kajiba-server.service")
        assert finished(r5) == "succeeded"
        attempt = attempts(r5)
        assert len(attempt) == 1 and attempt[0][2] == "succeeded", attempt
        text = log_of(attempt[0][0])
        assert text.count("long-start") == 1 and text.count("long-end") == 1, text
        size = forge.succeed(f"stat -c %s {logs_dir}/{attempt[0][0]}.log").strip()
        chunks = sql(f"SELECT COUNT(*), MIN(seq), MAX(seq), SUM(length) FROM log_chunk WHERE attempt_id = (SELECT id FROM job_attempt WHERE public_id = '{attempt[0][0]}')").split("|")
        assert chunks[0] == chunks[2] and chunks[1] == "1" and chunks[3] == size, (chunks, size)
        assert finishes(r5) == 1

    with subtest("a frozen runner loses its lease; the job is replayed and the late result ignored"):
        mark = int(sql("SELECT MAX(id) FROM event"))
        c6 = commit(LONG, "sixth")
        r6 = push(c6)
        started(r6)
        first = attempts(r6)[0][0]
        pid = runner.succeed("systemctl show -p MainPID --value kajiba-runner.service").strip()
        runner.succeed(f"kill -STOP {pid}")
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT state FROM job_attempt WHERE public_id = '{first}'\")\" = lost", timeout=120)
        forge.wait_until_succeeds(f"test $(sqlite3 {db} \"SELECT COUNT(*) FROM job_attempt a JOIN job ON job.id = a.job_id JOIN run ON run.id = job.run_id WHERE run.public_id = '{r6}'\") -ge 2")
        # Woken before the new lease ends: the old attempt still runs and reports late.
        runner.succeed(f"kill -CONT {pid}")
        assert finished(r6) == "succeeded"
        runner.wait_until_succeeds(f"journalctl _SYSTEMD_UNIT=kajiba-runner.service -o cat | grep attempt-finished | grep -F {first}")
        tries = attempts(r6)
        assert tries[0] == (first, "1", "lost") and tries[-1][2] == "succeeded", tries
        kinds = sql(f"SELECT kind FROM event WHERE id > {mark}").splitlines()
        assert "attempt-lost" in kinds and "attempt-replayed" in kinds, kinds
        assert finishes(r6) == 1

    with subtest("no connected runner: the job waits with its reason; ci run --detach returns at once"):
        runner.succeed("systemctl stop kajiba-runner.service")
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\")\" = 0")
        begin = time.monotonic()
        r7 = forge.succeed(f"{writer_cli} ci run --detach alice/demo {c1}").strip()
        assert time.monotonic() - begin < 10 and len(r7) == 26, r7
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT job.pending_reason FROM job JOIN run ON run.id = job.run_id WHERE run.public_id = '{r7}'\")\" = no-connected-runner")
        code, _ = forge.execute(f"{writer_cli} ci status {r7} >/tmp/r7.out 2>/tmp/r7.err")
        assert code == 75 and forge.succeed("cat /tmp/r7.out") == "queued\n", code
        assert f"run {r7} pending: no-connected-runner" in forge.succeed("cat /tmp/r7.err")
        runner.succeed("systemctl start kajiba-runner.service")
        forge.succeed(f"{writer_cli} ci watch {r7} >/dev/null 2>&1")
        assert finished(r7) == "succeeded"

    with subtest("the journal holds every CI event kind"):
        kinds = set(sql("SELECT DISTINCT kind FROM event").splitlines())
        expected = {"run-created", "run-started", "run-finished", "attempt-lost", "attempt-replayed", "runner-connected", "runner-disconnected", "runner-refused"}
        assert expected <= kinds, kinds

    with subtest("admin retention deletes old logs and keeps their runs"):
        runs = sql("SELECT COUNT(*) FROM run")
        first_attempt = attempts(r1)[0][0]
        forge.succeed(f"test -e {logs_dir}/{first_attempt}.log")
        out = forge.succeed("kajiba-server admin retention")
        assert out.startswith("logs deleted: ") and int(out.split()[2]) >= 1, out
        forge.fail(f"test -e {logs_dir}/{first_attempt}.log")
        assert sql(f"SELECT COUNT(*) FROM log_chunk WHERE attempt_id = (SELECT id FROM job_attempt WHERE public_id = '{first_attempt}')") == "0"
        assert sql("SELECT COUNT(*) FROM run") == runs
        assert sql(f"SELECT state FROM run WHERE public_id = '{r1}'") == "succeeded"

    validate_logs(forge, "kajiba-server.service")
    validate_logs(forge, "kajiba-git.service")
    validate_logs(runner, "kajiba-runner.service")
    validate_metrics(forge, metrics_url)
    validate_metrics(runner, "http://127.0.0.1:9432/metrics")
  '';
}
