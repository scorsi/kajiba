# J8 tasks 6/10: offline restore and cleanup recovery across both state trees.
{ pkgs, ... }@args:
import ./ci-common.nix args {
  name = "ops-restore";
  forgeConfig.environment.systemPackages = [ pkgs.python3 ];
  script = ''
    def restore_cmd(directory):
        return f"kajiba-server restore {directory} --config /etc/kajiba/server.toml"
    normal_query = "SELECT r.public_id, r.state, coalesce(r.failure_reason, 'none'), j.public_id, j.state, a.public_id, a.state, s.idx, s.state FROM run r JOIN job j ON j.run_id = r.id LEFT JOIN job_attempt a ON a.job_id = j.id LEFT JOIN step s ON s.attempt_id = a.id WHERE r.trigger != 'worktree' ORDER BY r.id, j.id, a.id, s.idx"
    units = "kajiba-server.service kajiba-git.service kajiba-server.socket kajiba-server-runner.socket kajiba-git.socket kajiba-git-ssh.socket"

    def stopped():
        forge.succeed("systemctl stop kajiba-dump.timer kajiba-gc.timer kajiba-dump.service kajiba-gc.service")
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
        old_run = push(commit("", "old log for cleanup"))
        assert finished(old_run) == "succeeded"
        old_attempt = attempts(old_run)[0][0]
        sha = forge.succeed("git -C /root/work rev-parse HEAD").strip()
        sql(f"UPDATE job_attempt SET finished_at = CAST(strftime('%s', 'now') AS INTEGER) * 1000 WHERE public_id = '{attempt}'")
        sql(f"UPDATE job_attempt SET finished_at = finished_at - 120000 WHERE public_id = '{old_attempt}'")
        normal_history = sql("SELECT (SELECT count(*) FROM run), (SELECT count(*) FROM job), (SELECT count(*) FROM job_attempt), (SELECT count(*) FROM step)")
        forge.succeed("kajiba-server admin retention")
        forge.succeed(f"test ! -e {logs_dir}/{old_attempt}.log")
        assert sql(f"SELECT count(*) FROM log_chunk JOIN job_attempt a ON a.id = attempt_id WHERE a.public_id = '{old_attempt}'") == "0"
        assert sql("SELECT (SELECT count(*) FROM run), (SELECT count(*) FROM job), (SELECT count(*) FROM job_attempt), (SELECT count(*) FROM step)") == normal_history
        assert sql(f"SELECT state FROM run WHERE public_id = '{old_run}'") == "succeeded"
        runner.succeed("systemctl stop kajiba-runner.service")
        forge.wait_until_succeeds(f"test $(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\") = 0")
        forge.succeed("git -C /root/work remote add origin https://forge/alice/demo.git")
        forge.succeed("echo restore-worktree >> /root/work/salt")
        active = forge.succeed(f"cd /root/work && {writer_cli} ci run --worktree --detach").strip().splitlines()[-1]
        assert sql(f"SELECT state FROM run WHERE public_id = '{active}'") == "queued", active
        forge.succeed(f"test -f /var/lib/kajiba-server/ci/bundles/{active}.bundle")

    with subtest("startup preserves archives and existing quarantines, moving only empty orphans"):
        stopped()
        archived_id = "00000000000000000000000007"
        reserved_id = "00000000000000000000000008"
        archived_dir = f"/var/lib/kajiba-git/repos/{archived_id}.git"
        quarantine = "/var/lib/kajiba-git/tmp/orphans"
        sql(f"INSERT INTO repo (public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, archived_at, config_key) SELECT '{archived_id}', owner_id, 'archive', visibility, 'off', default_branch, 'archive', 0, 1, 'archive' FROM repo WHERE config_key = 'demo'")
        forge.succeed(f"cp -a {repo_dir} {archived_dir}",
                      f"runuser -u kajiba-git -- git init -q --bare --shared=0640 /var/lib/kajiba-git/repos/{reserved_id}.git",
                      f"install -d -o kajiba-git -g kajiba-git -m 0750 {quarantine}/{reserved_id}.git.1",
                      f"printf preserved > {quarantine}/{reserved_id}.git.1/evidence",
                      f"chown -R kajiba-git:kajiba-git {quarantine}",
                      f"chmod 0640 {quarantine}/{reserved_id}.git.1/evidence",
                      "systemctl start kajiba-git.service kajiba-server.service")
        forge.wait_until_succeeds("curl -fsS http://127.0.0.1:8080/readyz")
        forge.succeed(f"test ! -e /var/lib/kajiba-git/repos/{reserved_id}.git",
                      f"test -f {quarantine}/{reserved_id}.git/HEAD",
                      f"test -f {quarantine}/{reserved_id}.git.1/evidence")
        assert forge.succeed(f"git --git-dir={archived_dir} rev-parse main").strip() == sha
        quarantine_files = forge.succeed(f"cd {quarantine} && find . -type f -exec sha256sum {{}} + | sort")
        assert sql(f"SELECT archived_at FROM repo WHERE public_id = '{archived_id}'") == "1"
        history_before_dump = sql(normal_query)
        forge.succeed("install -d -o kajiba-server -g kajiba-server -m 0700 /var/lib/kajiba-backup-handoff")
        forge.succeed("sh ${pkgs.writeText "kajiba-backup-hook.sh" (builtins.readFile ../../docs/examples/backup-hook.sh)}")
        copied = forge.succeed("find /var/lib/kajiba-backup-handoff -mindepth 1 -maxdepth 1 -type d").splitlines()
        assert len(copied) == 1, copied
        directory = copied[0]
        manifest_hash = forge.succeed(f"sha256sum {directory}/manifest.json").split()[0]

    with subtest("running daemons and stopped daemons with active sockets refuse restore"):
        code, out = forge.execute(restore_cmd(directory) + " 2>&1")
        assert code != 0 and "stop both daemons" in out, out
        forge.succeed("systemctl stop kajiba-server.service kajiba-git.service")
        # A service stop leaves activating sockets alive (0157).
        forge.succeed("systemctl is-active kajiba-server.socket")
        code, out = forge.execute(restore_cmd(directory) + " 2>&1")
        assert code != 0 and "activating sockets" in out, out
        stopped()

    with subtest("a newer schema refuses without changing either destination or the dump"):
        empty_destinations()
        forge.succeed(f"cp -a {directory} /root/newer-dump")
        # Rehash the modified DB so refusal exercises schema validation, not merely hashing.
        forge.succeed("sqlite3 /root/newer-dump/server/db/kajiba.sqlite \"INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) VALUES (9999, 'future', CURRENT_TIMESTAMP, 1, X'00', 0)\"")
        forge.succeed("python3 - <<'PY'\nimport base64, hashlib, json\nfrom pathlib import Path\np = Path('/root/newer-dump')\nm = json.loads((p / 'manifest.json').read_text())\nb = (p / 'server/db/kajiba.sqlite').read_bytes()\nk = base64.urlsafe_b64encode(b'server/db/kajiba.sqlite').decode().rstrip('=')\nm['entries'][k]['size'] = len(b)\nm['entries'][k]['sha256'] = hashlib.sha256(b).hexdigest()\nm['schema'] = 9999\n(p / 'manifest.json').write_text(json.dumps(m))\nPY")
        newer_hash = forge.succeed("sha256sum /root/newer-dump/server/db/kajiba.sqlite").split()[0]
        code, out = forge.execute(restore_cmd("/root/newer-dump") + " 2>&1")
        assert code != 0 and "schema" in out, out
        assert forge.succeed("sha256sum /root/newer-dump/server/db/kajiba.sqlite").split()[0] == newer_hash
        forge.succeed("test ! -e /var/lib/kajiba-server/db", "test ! -e /var/lib/kajiba-git/repos")

    with subtest("restore preserves the privilege split and clears absent log indexes"):
        forge.succeed(restore_cmd(directory))
        assert forge.succeed(f"sha256sum {directory}/manifest.json").split()[0] == manifest_hash
        for path, owner in [("/var/lib/kajiba-server/db", "kajiba-server:kajiba-server"), ("/var/lib/kajiba-git/repos", "kajiba-git:kajiba-git")]:
            assert forge.succeed(f"stat -c '%U:%G:%a' {path}").strip() == owner + ":750"
            forge.succeed(f"test -z \"$(find {path} -type d ! -perm 0750 -print -quit)\"",
                          f"test -z \"$(find {path} -type f ! -perm 0640 -print -quit)\"",
                          f"test -z \"$(find {path} ! -user {owner.split(':')[0]} -print -quit)\"")
        assert sql("SELECT count(*) FROM log_chunk") == "0"
        assert forge.succeed(f"cd {quarantine} && find . -type f -exec sha256sum {{}} + | sort") == quarantine_files
        assert forge.succeed(f"git --git-dir={archived_dir} rev-parse main").strip() == sha
        assert int(sql(f"SELECT purged_log_length FROM job_attempt WHERE public_id = '{attempt}'")) == old_length
        forge.succeed(f"test ! -e /var/lib/kajiba-server/ci/logs/{attempt}.log")
        forge.fail("runuser -u kajiba-server -- touch /var/lib/kajiba-git/repos/forbidden",
                   f"runuser -u kajiba-git -- cat {db}")
        forge.succeed("systemctl reset-failed " + units, "systemctl start kajiba-git.service kajiba-server.service")
        forge.wait_until_succeeds("curl -fsS http://127.0.0.1:8080/readyz")
        assert sql(f"SELECT state || '|' || failure_reason FROM run WHERE public_id = '{active}'") == "failed|bundle-missing"
        assert sql(f"SELECT state FROM run WHERE public_id = '{historical}'") == "succeeded"
        assert sql(normal_query) == history_before_dump
        forge.succeed(f"{writer_cli} ci logs {historical}")
        assert forge.succeed(f"git --git-dir={repo_dir} rev-parse main").strip() == sha
        forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git clone kajiba-git@127.0.0.1:alice/demo.git /root/restored-clone")
        assert forge.succeed("git -C /root/restored-clone rev-parse HEAD").strip() == sha
        runner.succeed("systemctl start kajiba-runner.service")
        runner.wait_for_unit("kajiba-runner.service")
        assert finished(push(commit("", "writes after restore"))) == "succeeded"

    with subtest("terminal worktree bundles go immediately; expiry removes only worktree history"):
        terminal = forge.succeed(f"cd /root/work && {writer_cli} ci run --worktree --detach").strip().splitlines()[-1]
        assert finished(terminal) == "succeeded"
        forge.wait_until_succeeds(f"test ! -e /var/lib/kajiba-server/ci/bundles/{terminal}.bundle")
        terminal_attempt = attempts(terminal)[0][0]
        forge.succeed(f"test -f {logs_dir}/{terminal_attempt}.log")
        runner.succeed("systemctl stop kajiba-runner.service")
        forge.wait_until_succeeds(f"test $(sqlite3 {db} \"SELECT connected FROM runner WHERE name = 'build-1'\") = 0")
        queued = forge.succeed(f"cd /root/work && {writer_cli} ci run --worktree --detach").strip().splitlines()[-1]
        forge.succeed(f"test -f /var/lib/kajiba-server/ci/bundles/{queued}.bundle")
        assert sql(f"SELECT state FROM run WHERE public_id = '{queued}'") == "queued"
        # Age only worktree runs; normal history, including steps and failure reasons, stays.
        normal_history = sql(normal_query)
        sql("UPDATE run SET created_at = created_at - 172800000 WHERE trigger = 'worktree'")
        forge.succeed("kajiba-server admin retention", "kajiba-server admin retention")
        assert sql("SELECT count(*) FROM run WHERE trigger = 'worktree'") == "0"
        assert sql(normal_query) == normal_history
        forge.succeed(f"test ! -e /var/lib/kajiba-server/ci/bundles/{queued}.bundle",
                      f"test ! -e {logs_dir}/{terminal_attempt}.log")
        assert sql("PRAGMA foreign_key_check") == ""
        assert forge.succeed(f"cd {quarantine} && find . -type f -exec sha256sum {{}} + | sort") == quarantine_files
        runner.succeed("systemctl start kajiba-runner.service")
        runner.wait_for_unit("kajiba-runner.service")

    with subtest("populated and malformed orphans refuse startup without moving or deleting history"):
        for name, populated in [("populated.git", True), ("malformed.git", False), ("malformed-head.git", False)]:
            stopped()
            orphan = "/var/lib/kajiba-git/repos/" + name
            if populated:
                forge.succeed(f"cp -a {repo_dir} {orphan}")
            elif name == "malformed-head.git":
                forge.succeed(f"runuser -u kajiba-git -- git init -q --bare --shared=0640 {orphan}",
                              f"printf 'not a ref\\n' > {orphan}/HEAD")
            else:
                forge.succeed(f"install -d -o kajiba-git -g kajiba-git -m 0750 {orphan}",
                              f"printf malformed > {orphan}/evidence",
                              f"chown kajiba-git:kajiba-git {orphan}/evidence")
            before = forge.succeed(f"cd {orphan} && find . -type f -exec sha256sum {{}} + | sort")
            forge.succeed("systemctl reset-failed " + units, "systemctl start kajiba-git.service")
            code, out = forge.execute("timeout 60 systemctl start kajiba-server.service")
            assert code not in (0, 124), out
            stopped()
            assert forge.succeed(f"cd {orphan} && find . -type f -exec sha256sum {{}} + | sort") == before
            forge.succeed(f"test ! -e {quarantine}/{name}")
            assert forge.succeed(f"cd {quarantine} && find . -type f -exec sha256sum {{}} + | sort") == quarantine_files
            # Only the operator removes the test's ineligible orphan; then startup retries.
            forge.succeed(f"rm -rf {orphan}", "systemctl reset-failed " + units,
                          "systemctl start kajiba-git.service kajiba-server.service")
            forge.wait_until_succeeds("curl -fsS http://127.0.0.1:8080/readyz")
            assert forge.succeed(f"git --git-dir={archived_dir} rev-parse main").strip() == sha
            assert sql(f"SELECT archived_at FROM repo WHERE public_id = '{archived_id}'") == "1"
  '';
}
