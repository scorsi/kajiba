# J8 task 6: offline restore, ownership, activation refusal and excluded CI recovery.
args:
import ./ci-common.nix args {
  name = "ops-restore";
  script = ''
    restore_cmd = "kajiba-server restore --config /etc/kajiba/server.toml "
    units = "kajiba-server.service kajiba-git.service kajiba-server.socket kajiba-server-runner.socket kajiba-git.socket kajiba-git-ssh.socket"

    def stopped():
        forge.succeed("systemctl stop " + units)
        for unit in units.split():
            assert forge.succeed(f"systemctl show -P ActiveState {unit}").strip() in ("inactive", "failed")

    def empty_destinations():
        forge.succeed("find /var/lib/kajiba-server -mindepth 1 -maxdepth 1 ! -name kajiba.lock -exec rm -rf {} +",
                      "find /var/lib/kajiba-git -mindepth 1 -maxdepth 1 ! -name kajiba-git.lock -exec rm -rf {} +")

    with subtest("seed historical logs and an active worktree run, then dump"):
        sha = commit("", "history before restore")
        historical = push(sha)
        assert finished(historical) == "succeeded"
        attempt = attempts(historical)[0][0]
        old_length = int(sql(f"SELECT max(offset + length) FROM log_chunk WHERE attempt_id = (SELECT id FROM job_attempt WHERE public_id = '{attempt}')"))
        assert old_length > 0
        runner.succeed("systemctl stop kajiba-runner.service")
        forge.wait_until_succeeds(f"test $(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\") = 0")
        forge.succeed("git -C /root/work remote add origin https://forge/alice/demo.git")
        forge.succeed("echo restore-worktree >> /root/work/salt")
        active = forge.succeed(f"cd /root/work && {writer_cli} ci run --worktree --detach").strip().splitlines()[-1]
        assert sql(f"SELECT state FROM run WHERE public_id = '{active}'") == "queued", active
        forge.succeed(f"test -f /var/lib/kajiba-server/ci/bundles/{active}.bundle")
        directory = forge.succeed("kajiba-server admin dump").strip()
        manifest_hash = forge.succeed(f"sha256sum {directory}/manifest.json").split()[0]

    with subtest("running daemons and stopped daemons with active sockets refuse restore"):
        code, out = forge.execute(restore_cmd + directory + " 2>&1")
        assert code != 0 and "stop both daemons" in out, out
        forge.succeed("systemctl stop kajiba-server.service kajiba-git.service")
        # A service stop leaves activating sockets alive (0157).
        forge.succeed("systemctl is-active kajiba-server.socket")
        code, out = forge.execute(restore_cmd + directory + " 2>&1")
        assert code != 0 and "activating sockets" in out, out
        stopped()

    with subtest("a newer schema refuses without changing either destination or the dump"):
        empty_destinations()
        forge.succeed(f"cp -a {directory} /root/newer-dump")
        # Rehash the modified DB so refusal exercises schema validation, not merely hashing.
        forge.succeed("sqlite3 /root/newer-dump/server/db/kajiba.sqlite \"INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) VALUES (9999, 'future', CURRENT_TIMESTAMP, 1, X'00', 0)\"")
        forge.succeed("python3 - <<'PY'\nimport base64, hashlib, json\nfrom pathlib import Path\np = Path('/root/newer-dump')\nm = json.loads((p / 'manifest.json').read_text())\nb = (p / 'server/db/kajiba.sqlite').read_bytes()\nk = base64.urlsafe_b64encode(b'server/db/kajiba.sqlite').decode().rstrip('=')\nm['entries'][k]['size'] = len(b)\nm['entries'][k]['sha256'] = hashlib.sha256(b).hexdigest()\nm['schema'] = 9999\n(p / 'manifest.json').write_text(json.dumps(m))\nPY")
        newer_hash = forge.succeed("sha256sum /root/newer-dump/server/db/kajiba.sqlite").split()[0]
        code, out = forge.execute(restore_cmd + "/root/newer-dump 2>&1")
        assert code != 0 and "schema" in out, out
        assert forge.succeed("sha256sum /root/newer-dump/server/db/kajiba.sqlite").split()[0] == newer_hash
        forge.succeed("test ! -e /var/lib/kajiba-server/db", "test ! -e /var/lib/kajiba-git/repos")

    with subtest("restore preserves the privilege split and clears absent log indexes"):
        forge.succeed(restore_cmd + directory)
        assert forge.succeed(f"sha256sum {directory}/manifest.json").split()[0] == manifest_hash
        for path, owner in [("/var/lib/kajiba-server/db", "kajiba-server:kajiba-server"), ("/var/lib/kajiba-git/repos", "kajiba-git:kajiba-git")]:
            assert forge.succeed(f"stat -c '%U:%G:%a' {path}").strip() == owner + ":750"
            forge.succeed(f"test -z \"$(find {path} -type d ! -perm 0750 -print -quit)\"",
                          f"test -z \"$(find {path} -type f ! -perm 0640 -print -quit)\"",
                          f"test -z \"$(find {path} ! -user {owner.split(':')[0]} -print -quit)\"")
        assert sql("SELECT count(*) FROM log_chunk") == "0"
        assert int(sql(f"SELECT purged_log_length FROM job_attempt WHERE public_id = '{attempt}'")) == old_length
        forge.succeed(f"test ! -e /var/lib/kajiba-server/ci/logs/{attempt}.log")
        forge.fail("runuser -u kajiba-server -- touch /var/lib/kajiba-git/repos/forbidden",
                   f"runuser -u kajiba-git -- cat {db}")
        forge.succeed("systemctl reset-failed " + units, "systemctl start kajiba-git.service kajiba-server.service")
        forge.wait_until_succeeds("curl -fsS http://127.0.0.1:8080/readyz")
        assert sql(f"SELECT state || '|' || failure_reason FROM run WHERE public_id = '{active}'") == "failed|bundle-missing"
        assert sql(f"SELECT state FROM run WHERE public_id = '{historical}'") == "succeeded"
        forge.succeed(f"{writer_cli} ci logs {historical}")
        assert forge.succeed(f"git --git-dir={repo_dir} rev-parse main").strip() == sha
        forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git clone kajiba-git@127.0.0.1:alice/demo.git /root/restored-clone")
        assert forge.succeed("git -C /root/restored-clone rev-parse HEAD").strip() == sha
        runner.succeed("systemctl start kajiba-runner.service")
        runner.wait_for_unit("kajiba-runner.service")
        assert finished(push(commit("", "writes after restore"))) == "succeeded"
  '';
}
