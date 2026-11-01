# J8 task 13: recover both lost state trees with deployment configuration and secrets intact.
{ pkgs, ... }@args:
import ./ci-common.nix args {
  name = "ops";
  forgeConfig = {
    environment.systemPackages = [ pkgs.python3 ];
    services.kajiba-git.metrics.listen = "127.0.0.1:9433";
    # Recovery assertions must not race the fixture's one-minute log retention.
    services.kajiba-server.ci.logRetention = pkgs.lib.mkForce "1d";
  };
  script = ''
    passkey = "python3 ${./fixtures/ops-passkey.py}"
    units = "kajiba-server.service kajiba-git.service kajiba-server.socket kajiba-server-runner.socket kajiba-git.socket kajiba-git-ssh.socket"
    history_query = "SELECT r.public_id, r.state, coalesce(r.failure_reason, 'none'), j.public_id, j.state, a.public_id, a.state, s.idx, s.state FROM run r JOIN job j ON j.run_id = r.id LEFT JOIN job_attempt a ON a.job_id = j.id LEFT JOIN step s ON s.attempt_id = a.id WHERE r.trigger != 'worktree' ORDER BY r.id, j.id, a.id, s.idx"
    identity_queries = [
        "SELECT id, public_id, name, kind, is_admin, source, disabled_at FROM user ORDER BY id",
        "SELECT id, public_id, owner_id, name, archived_at FROM repo ORDER BY id",
        "SELECT public_id, user_id, fingerprint, usage, source FROM ssh_key ORDER BY id",
        "SELECT public_id, user_id, name, hex(token_hash), scopes, source, revoked_at FROM api_token ORDER BY id",
        "SELECT public_id, user_id, hex(credential_id), name FROM webauthn_credential ORDER BY id",
        "SELECT id, public_id, name FROM runner ORDER BY id",
    ]

    def rpc(service, method, data, auth, success=True):
        command = f"grpcurl -plaintext -H {shlex.quote(auth)} -import-path {proto} -proto kajiba/v1/{service}.proto -d {shlex.quote(json.dumps(data))} 127.0.0.1:8080 kajiba.v1.{method} 2>&1"
        code, out = forge.execute(command)
        assert (code == 0) == success, (code, out)
        return json.loads(out) if success else out

    declared = "authorization: Bearer " + forge.succeed("sed -n 2p /root/writer-token").strip()

    def forge_refs(path):
        return forge.succeed(f"git --git-dir={path} symbolic-ref HEAD; git --git-dir={path} show-ref --head | sort")

    def clone_content(path):
        return forge.succeed(f"git -C {path} rev-parse HEAD; git -C {path} ls-tree -r HEAD; cd {path} && sha256sum blob salt flake.nix flake.lock")

    with subtest("seed a real passkey, declared and runtime PATs, and revoked credentials"):
        link = forge.succeed("kajiba-server admin enroll writer").strip().splitlines()[-1]
        cookie = forge.succeed(f"{passkey} enroll /root/authenticator {shlex.quote(link.split('#', 1)[1])}").strip()
        cookie = forge.succeed(f"{passkey} login /root/authenticator").strip()
        session = "cookie: " + cookie
        assert rpc("auth", "AuthService/GetViewer", {}, session)["user"] == "writer"
        token = rpc("token", "TokenService/CreateToken", {"name": "recovery", "scopes": ["SCOPE_REPO_READ"]}, session)
        runtime = "authorization: Bearer " + token["secret"]
        revoked = rpc("token", "TokenService/CreateToken", {"name": "revoked", "scopes": ["SCOPE_REPO_READ"]}, session)
        rpc("token", "TokenService/RevokeToken", {"publicId": revoked["token"]["publicId"]}, session)
        assert rpc("auth", "AuthService/GetViewer", {}, runtime)["user"] == "writer"
        rpc("auth", "AuthService/GetViewer", {}, "authorization: Bearer " + revoked["secret"], success=False)
        # Scope enforcement is exercised before and after recovery.
        out = rpc("run", "RunService/TriggerRun", {"repo": "alice/demo", "sha": "0" * 40}, runtime, success=False)
        assert has_reason(out, "SCOPE_MISSING"), out

    with subtest("seed branches, signed tags, archived content and completed success/failure history"):
        good = push(commit("", "successful history"))
        assert finished(good) == "succeeded"
        fail = '        fail = check "fail" "echo disaster-fixture-failure; exit 3";\n'
        bad = push(commit(fail, "failed history"))
        assert finished(bad) == "failed"
        failure = rpc("run", "RunService/GetRun", {"run": bad}, declared)["run"]["failure"]
        assert failure["reason"] and "disaster-fixture-failure" in "\n".join(failure.get("lastLogLines", [])), failure
        tip = commit("", "recovery point")
        current = push(tip)
        assert finished(current) == "succeeded"
        forge.succeed("git -C /root/work branch recovery-topic",
                      "git -C /root/work tag -s recovery-v1 -m recovery-v1",
                      f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git -C /root/work push kajiba-git@127.0.0.1:alice/demo.git refs/heads/recovery-topic refs/tags/recovery-v1")
        forge.wait_until_succeeds(f"test $(sqlite3 {db} \"SELECT count(*) FROM run WHERE state IN ('queued', 'running')\") = 0")
        archived_id = "00000000000000000000000007"
        archived_dir = f"/var/lib/kajiba-git/repos/{archived_id}.git"
        sql(f"INSERT INTO repo (public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, archived_at, config_key) SELECT '{archived_id}', owner_id, 'archive', visibility, 'off', default_branch, 'archive', 0, 1, 'archive' FROM repo WHERE config_key = 'demo'")
        forge.succeed(f"cp -a {repo_dir} {archived_dir}")
        expected_refs = {path: forge_refs(path) for path in [repo_dir, archived_dir]}
        expected_content = clone_content("/root/work")
        history = sql(history_query)
        assert sql("SELECT count(*) FROM step") != "0"
        identities = [sql(query) for query in identity_queries]
        deployment_files = "/etc/kajiba/server.toml /etc/ssh/kajiba_host_ed25519_key /var/lib/hanko/kajiba-server/*.pem"
        deployment = forge.succeed("sha256sum " + deployment_files)
        log_length = int(sql("SELECT sum(length) FROM log_chunk"))
        assert log_length > 0

    with subtest("dump an active worktree job without waiting for its independent unit"):
        forge.succeed("git -C /root/work remote add origin https://forge/alice/demo.git")
        long = '        long = check "long" "echo long-start; /tools/sleep 90; echo long-end; echo > $out";\n'
        text = FLAKE.replace("EXTRA", long)
        forge.succeed(f"printf %s {shlex.quote(text)} > /root/work/flake.nix")
        active = forge.succeed(f"cd /root/work && {writer_cli} ci run --worktree --detach").strip().splitlines()[-1]
        started(active)
        stale = attempts(active)[0][0]
        unit = f"kajiba-job-{stale}.service"
        runner.succeed("systemctl stop kajiba-runner.service", f"systemctl is-active {unit}")
        forge.succeed("install -d -o kajiba-server -g kajiba-server -m 0700 /var/lib/kajiba-backup-handoff",
                      "sh ${pkgs.writeText "kajiba-backup-hook.sh" (builtins.readFile ../../docs/examples/backup-hook.sh)}")
        copied = forge.succeed("find /var/lib/kajiba-backup-handoff -mindepth 1 -maxdepth 1 -type d").splitlines()
        assert len(copied) == 1, copied
        directory = copied[0]
        runner.succeed(f"systemctl is-active {unit}")
        assert sql(f"SELECT state FROM run WHERE public_id = '{active}'") == "running"
        manifest = forge.succeed(f"sha256sum {directory}/manifest.json")

    with subtest("stop documented units, lose both state trees, restore and start normally"):
        forge.succeed("systemctl stop kajiba-dump.timer kajiba-gc.timer kajiba-dump.service kajiba-gc.service",
                      "systemctl stop " + units)
        for name in units.split():
            assert forge.succeed(f"systemctl show -P ActiveState {name}").strip() in ("inactive", "failed")
        # A result created after the recovery point remains on the disconnected runner.
        runner.wait_until_succeeds(f"test -s /var/lib/kajiba-runner/jobs/{stale}/result.json", timeout=150)
        runner.succeed(f"test -s /var/lib/kajiba-runner/jobs/{stale}/state.json")
        forge.succeed("find /var/lib/kajiba-server -mindepth 1 -maxdepth 1 ! -name kajiba.lock -exec rm -rf {} +",
                      "find /var/lib/kajiba-git -mindepth 1 -maxdepth 1 ! -name kajiba-git.lock -exec rm -rf {} +",
                      "install -d -o kajiba-server -g kajiba-server -m 0750 /var/lib/kajiba-server",
                      "install -d -o kajiba-git -g kajiba-git -m 0750 /var/lib/kajiba-git",
                      f"kajiba-server restore {directory} --config /etc/kajiba/server.toml")
        assert forge.succeed(f"sha256sum {directory}/manifest.json") == manifest
        assert sql("SELECT count(*) FROM log_chunk") == "0"
        forge.succeed("systemctl reset-failed " + units,
                      "systemctl start kajiba-git.service kajiba-server.service",
                      "curl -fsS http://127.0.0.1:8080/readyz",
                      "systemctl start kajiba-dump.timer kajiba-gc.timer")
        assert [sql(query) for query in identity_queries] == identities
        assert forge.succeed("sha256sum " + deployment_files) == deployment
        assert sql(history_query) == history
        assert sql(f"SELECT state || '|' || failure_reason FROM run WHERE public_id = '{active}'") == "failed|bundle-missing"
        recovered_active = sql(history_query.replace("r.trigger != 'worktree'", f"r.public_id = '{active}'"))
        assert sql("PRAGMA foreign_key_check") == ""
        for path, expected in expected_refs.items():
            assert forge_refs(path) == expected
        assert sql(f"SELECT archived_at FROM repo WHERE public_id = '{archived_id}'") == "1"

    with subtest("HTTP and SSH clones recover identical HEAD, refs and file content"):
        forge.succeed("git -c credential.helper='!f() { echo username=writer; echo password=$(sed -n 2p /root/writer-token); }; f' clone https://forge/alice/demo.git /root/http-clone",
                      f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git clone kajiba-git@127.0.0.1:alice/demo.git /root/ssh-clone")
        for path in ["/root/http-clone", "/root/ssh-clone"]:
            assert clone_content(path) == expected_content
            assert forge.succeed(f"git -C {path} rev-parse refs/remotes/origin/recovery-topic").strip() == tip
            assert forge.succeed(f"git -C {path} rev-parse refs/tags/recovery-v1").strip() == forge.succeed("git -C /root/work rev-parse refs/tags/recovery-v1").strip()

    with subtest("a fresh passkey assertion authenticates; PAT authorization and revocation survive"):
        cookie = forge.succeed(f"{passkey} login /root/authenticator").strip()
        session = "cookie: " + cookie
        assert rpc("auth", "AuthService/GetViewer", {}, session)["user"] == "writer"
        for auth in [declared, runtime]:
            assert rpc("auth", "AuthService/GetViewer", {}, auth)["user"] == "writer"
        out = rpc("run", "RunService/TriggerRun", {"repo": "alice/demo", "sha": tip}, runtime, success=False)
        assert has_reason(out, "SCOPE_MISSING"), out
        rpc("auth", "AuthService/GetViewer", {}, "authorization: Bearer " + revoked["secret"], success=False)
        rpc("token", "TokenService/RevokeToken", {"publicId": token["token"]["publicId"]}, session)
        rpc("auth", "AuthService/GetViewer", {}, runtime, success=False)
        # Declared tokens remain usable after runtime revocation.
        assert rpc("auth", "AuthService/GetViewer", {}, declared)["user"] == "writer"

    with subtest("history stays visible with steps and failure summaries but without ephemeral logs"):
        visible = rpc("run", "RunService/ListRuns", {"repo": "alice/demo"}, declared)["runs"]
        assert {good, bad, current, active}.issubset({run["publicId"] for run in visible}), visible
        recovered = rpc("run", "RunService/GetRun", {"run": bad}, declared)["run"]
        assert recovered["failure"]["reason"] == failure["reason"]
        assert not recovered["failure"].get("lastLogLines"), recovered
        assert recovered["job"]["currentAttempt"]["state"] == "ATTEMPT_STATE_FAILED"
        assert sql(history_query) == history
        for run_id in [good, bad, current, active]:
            forge.succeed(f"{writer_cli} ci logs {run_id}")
        forge.succeed(f"test -z \"$(find {logs_dir} -type f -name '*.log' -print -quit)\"")

    with subtest("stale runner completion cannot change restored history; a new signed push builds"):
        terminal = sql(f"SELECT state, failure_reason FROM run WHERE public_id = '{active}'")
        runner.succeed("systemctl start kajiba-runner.service")
        runner.wait_for_unit("kajiba-runner.service")
        runner.wait_until_succeeds(f"test ! -d /var/lib/kajiba-runner/jobs/{stale}")
        runner.fail(f"systemctl is-active --quiet {unit}")
        assert sql(f"SELECT state, failure_reason FROM run WHERE public_id = '{active}'") == terminal
        assert attempts(active) == [(stale, "1", "failed")]
        assert finishes(active) == 1
        assert sql(history_query.replace("r.trigger != 'worktree'", f"r.public_id = '{active}'")) == recovered_active
        assert sql(history_query) == history
        fresh = push(commit("", "signed push after disaster recovery"))
        assert finished(fresh) == "succeeded"
        assert len(attempts(fresh)) == 1 and finishes(fresh) == 1
        assert "pass-ok" in forge.succeed(f"{writer_cli} ci logs {fresh}")
        assert sql(f"SELECT state, failure_reason FROM run WHERE public_id = '{active}'") == terminal
        validate_metrics(forge, "http://127.0.0.1:9433/metrics")
        # ci-common validates both daemon journals, the runner journal and server/runner metrics.
  '';
}
