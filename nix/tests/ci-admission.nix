# J6 task 20, runner mTLS task 8: runner admission by SVID through the SNI relay and every
# refusal (identity, another CA, version, capabilities; disabled is in `nixos-ci-worktree`), the
# public port without kajiba.runner.v1, invalid runner configurations, SVIDs reissued in place,
# the pending reasons and the journal (0124, 0125, 0126).
{ pkgs, kajibaRunnerPackage, ... }@args:
let
  inherit (pkgs) lib;
  svid = import ./svid.nix { inherit pkgs; };
  runnerId = svid.id "runner" "kajiba-runner";
  version = kajibaRunnerPackage.version;
  otherVersion = if builtins.elemAt (lib.splitString "." version) 1 == "2" then "0.3.0" else "0.2.0";
in
import ./ci-common.nix args {
  name = "ci-admission";
  script = ''
    import time

    with subtest("the runner connects over HTTPS and shows its capabilities and OS hostname"):
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\")\" = 1")
        row = sql("SELECT version, hostname, capabilities FROM runner WHERE name = 'build-1'").split("|", 2)
        assert row[0] == "${version}" and row[1] == "runner", row
        capabilities = json.loads(row[2])
        assert capabilities["arch"] == "${pkgs.stdenv.hostPlatform.parsed.cpu.name}" and capabilities["os"] == "linux", capabilities
        assert capabilities["max_jobs"] == 2, capabilities
        assert metric(forge, metrics_url, "kajiba_runners_connected") == 1

    with subtest("an undeclared ID, another CA, an extra capability and another 0.minor are refused"):
        identity = 'kajiba_runner_refusals_total{reason="identity"}'
        # The real runner with copies of other SVIDs: one of the trust domain that no runner
        # declares, and the runner's own ID from another CA.
        for name, identity_dir in [("stranger", "stranger"), ("foreign", "other-ca/runner")]:
            runner.succeed(
                f"install -d -o kajiba-runner -g kajiba-runner -m 0700 {manual}/{name}",
                f"install -o kajiba-runner -g kajiba-runner -m 0600 ${svid.ca}/{identity_dir}/svid.pem ${svid.ca}/{identity_dir}/key.pem ${svid.ca}/bundle.pem {manual}/{name}/",
            )
        code, out = manual_runner(runner_toml("", files=f"{manual}/stranger"), 8)
        assert code == 124 and "Unauthenticated" in out, out
        assert metric(forge, metrics_url, identity) >= 1
        refused = [line for line in unit_lines(forge, "kajiba-server.service") if line.get("event") == "runner-refused" and line.get("reason") == "identity"]
        assert any(line.get("spiffe_id") == "${svid.id "stranger" "kajiba-runner"}" for line in refused), refused
        # Another CA's certificate never reaches the service: the handshake fails, no refusal is counted.
        before = metric(forge, metrics_url, identity)
        code, out = manual_runner(runner_toml("", files=f"{manual}/foreign"), 8)
        assert code == 124 and "server-disconnected" in out and "Unauthenticated" not in out, out
        assert metric(forge, metrics_url, identity) == before
        out = hello("${otherVersion}", [])
        assert "FailedPrecondition" in out and has_reason(out, "VERSION_INCOMPATIBLE"), out
        out = hello("${version}", ["kvm"])
        assert "InvalidArgument" in out and has_reason(out, "INVALID_FIELD"), out
        # The real runner, announcing a tag the server did not declare.
        code, out = manual_runner(runner_toml('tags = ["kvm"]\n'), 8)
        assert code == 124 and "InvalidArgument" in out, out
        refusals = [line.get("reason") for line in unit_lines(forge, "kajiba-server.service") if line.get("event") == "runner-refused"]
        assert {"identity", "version", "capabilities"} <= set(refusals), refusals
        for reason in ["identity", "version", "capabilities"]:
            assert metric(forge, metrics_url, f'kajiba_runner_refusals_total{{reason="{reason}"}}') >= 1
        # E11: an unauthenticated refusal is logged and counted, never journaled.
        journaled = [json.loads(p)["reason"] for p in sql("SELECT payload FROM event WHERE kind = 'runner-refused'").splitlines()]
        assert "identity" not in journaled and {"version", "capabilities"} <= set(journaled), journaled
        # The declared runner kept its connection.
        assert sql("SELECT connected FROM runner WHERE name = 'build-1'") == "1"

    with subtest("the public port does not serve kajiba.runner.v1"):
        # Directly and through the HTTP front the relay picks for any other name.
        for flags, address in [("-plaintext", "127.0.0.1:8080"), ("", "forge:443")]:
            code, out = forge.execute(f"grpcurl {flags} -import-path {proto} -proto kajiba/runner/v1/runner.proto -d '{{}}' {address} kajiba.runner.v1.RunnerService/FetchRunBundle 2>&1")
            assert code != 0 and "Unimplemented" in out, (address, out)
        # The front saw kajiba.runner.v1 only from this probe: the runner passes the relay.
        requests = [line.split() for line in forge.succeed("cat /var/log/nginx/kajiba.log").splitlines()]
        probes = [r for r in requests if r[1].startswith("/kajiba.runner.v1.")]
        assert len(probes) == 1 and probes[0][1] == "/kajiba.runner.v1.RunnerService/FetchRunBundle", probes

    with subtest("plain http and a host without the sandbox refuse to start"):
        plain = runner_toml("").replace("https://", "http://")
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

    with subtest("SVIDs reissued in place are used at the next handshake, without a restart"):
        pids = [m.succeed(f"systemctl show -p MainPID --value {unit}").strip() for m, unit in [(runner, "kajiba-runner.service"), (forge, "kajiba-server.service")]]
        connects = int(sql("SELECT COUNT(*) FROM event WHERE kind = 'runner-connected'"))
        old = runner.succeed("${pkgs.openssl}/bin/openssl x509 -noout -serial -in /var/lib/hanko/kajiba-runner/svid.pem").strip()
        assert reissue(runner, "kajiba-runner", "${runnerId}") != old
        reissue(forge, "kajiba-server", "${svid.serverId}")
        # Restarting the relay cuts the runner's connection; it reconnects with a new handshake.
        forge.succeed("systemctl restart nginx.service")
        forge.wait_until_succeeds(f"test $(sqlite3 {db} \"SELECT COUNT(*) FROM event WHERE kind = 'runner-connected'\") -gt {connects}")
        for m, unit in [(runner, "kajiba-runner.service"), (forge, "kajiba-server.service")]:
            m.wait_until_succeeds(f"journalctl --sync; journalctl _SYSTEMD_UNIT={unit} -o cat | grep -q svid-reloaded")
        assert pids == [m.succeed(f"systemctl show -p MainPID --value {unit}").strip() for m, unit in [(runner, "kajiba-runner.service"), (forge, "kajiba-server.service")]]
        assert sql("SELECT connected FROM runner WHERE name = 'build-1'") == "1"

    with subtest("a job waits with its reason: no connected runner, then none with its backend"):
        # A commit reached without a push (no run), for a manual run.
        c1 = commit("", "admission", sign=False)
        forge.succeed(
            "git -C /root/work bundle create /var/lib/kajiba-fixtures/admission.bundle main",
            "chmod 0644 /var/lib/kajiba-fixtures/admission.bundle",
            f"runuser -u kajiba-git -- env HOME=/var/empty GIT_CONFIG_GLOBAL=/dev/null git --git-dir={repo_dir} fetch -q /var/lib/kajiba-fixtures/admission.bundle main:refs/contrib/writer/admission",
        )
        runner.succeed("systemctl stop kajiba-runner.service")
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\")\" = 0")
        # ci run --detach returns at once.
        begin = time.monotonic()
        r7 = forge.succeed(f"{writer_cli} ci run --detach alice/demo {c1}").strip()
        assert time.monotonic() - begin < 10 and len(r7) == 26, r7
        reason = f"SELECT job.pending_reason FROM job JOIN run ON run.id = job.run_id WHERE run.public_id = '{r7}'"
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"{reason}\")\" = no-connected-runner")
        code, _ = forge.execute(f"{writer_cli} ci status {r7} >/tmp/r7.out 2>/tmp/r7.err")
        assert code == 75 and forge.succeed("cat /tmp/r7.out") == "queued\n", code
        assert f"run {r7} pending: no-connected-runner" in forge.succeed("cat /tmp/r7.err")
        # The runner announcing no backend is admitted and cannot take the job.
        config = runner_toml("backends = []\n")
        runner.succeed(
            f"printf %s {shlex.quote(config)} > {manual}/runner.toml",
            f"(runuser -u kajiba-runner -- env PATH=/run/current-system/sw/bin HOME={manual} timeout 120 kajiba-runner --config {manual}/runner.toml) </dev/null >/dev/null 2>&1 &",
        )
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"{reason}\")\" = backend-unavailable")
        code, _ = forge.execute(f"{writer_cli} ci status {r7} >/dev/null 2>/tmp/r7.err")
        assert code == 75 and f"run {r7} pending: backend-unavailable" in forge.succeed("cat /tmp/r7.err"), code
        runner.succeed(f"pkill -u kajiba-runner -f 'kajiba-runner --config {manual}/runner.toml'")
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\")\" = 0")
        runner.succeed("systemctl start kajiba-runner.service")
        forge.succeed(f"{writer_cli} ci watch {r7} >/dev/null 2>&1")
        assert finished(r7) == "succeeded"

    with subtest("the journal holds every runner and run event kind"):
        kinds = set(sql("SELECT DISTINCT kind FROM event").splitlines())
        expected = {"run-created", "run-started", "run-finished", "runner-connected", "runner-disconnected", "runner-refused"}
        assert expected <= kinds, kinds
  '';
}
