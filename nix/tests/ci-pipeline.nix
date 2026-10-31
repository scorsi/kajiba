# J6 task 20: the nominal CI path. A signed push runs `nix flake check` and shows its run, success
# and failure with the stderr summary, refs/contrib without a run but a manual trigger, a read
# user refused, the CLI (`ci list`, `watch`, `status`, `logs`, `run --detach`), a repository in
# `manual` mode (0133), sources only as bundles, log retention.
args:
import ./ci-common.nix args {
  name = "ci-pipeline";
  forgeConfig.services.kajiba-server.repos.manual = {
    owner = "alice";
    description = "CI on demand";
    ci = "manual";
    permissions.writer = "write";
  };
  script = ''
    import time

    # More than 64 KiB of output: several log chunks, and enough to close a pipe early.
    FAIL = '        fail = check "fail" "i=0; while [ $i -lt 3000 ]; do i=$((i+1)); echo fail-line-$i-0123456789abcdef0123456789abcdef0123456789abcdef; done; exit 3";\n'


    def status(run_id):
        code, out = forge.execute(f"{writer_cli} ci status {run_id} 2>/dev/null")
        return code, out


    with subtest("a signed push on main runs the checks and succeeds"):
        c1 = commit("", "first")
        r1 = push(c1)
        out = forge.succeed(f"{writer_cli} ci watch {r1} 2>/tmp/r1.err")
        assert "pass-ok" in out, out
        assert finished(r1) == "succeeded"
        r1_done = time.monotonic()
        assert "attempt 1 on build-1 (runner)" in forge.succeed("cat /tmp/r1.err")
        assert status(r1) == (0, "succeeded\n")

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
        assert status(r2) == (1, "failed\n")
        # Manual run of the same commit: `ci run` follows and exits 1.
        code, _ = forge.execute(f"{writer_cli} ci run alice/demo {c2} 2>/tmp/r2-manual.err")
        assert code == 1, forge.succeed("cat /tmp/r2-manual.err")

    with subtest("a closed pipe ends kajiba ci logs quietly"):
        assert forge.succeed(f"set -o pipefail; {writer_cli} ci logs {r2} 2>/tmp/head.err | head -c 4096").strip() != ""
        # Attempt markers go to stderr by design (0117); nothing else may, e.g. a panic or EPIPE error.
        head_err = forge.succeed("cat /tmp/head.err")
        assert all(line.startswith("kajiba: attempt ") for line in head_err.splitlines()), head_err

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

    with subtest("kajiba ci list shows the repository's runs, newest first, filtered by state"):
        ids = sql("SELECT run.public_id FROM run JOIN repo ON repo.id = run.repo_id WHERE repo.config_key = 'demo' ORDER BY run.id DESC").splitlines()
        rows = [line.split() for line in forge.succeed(f"{writer_cli} ci list --repo alice/demo").splitlines()]
        assert len(ids) == 4 and [row[0] for row in rows] == ids, (ids, rows)
        assert rows[-1][:5] == [r1, "succeeded", "push", c1[:12], "refs/heads/main"], rows
        rows = [line.split() for line in forge.succeed(f"{writer_cli} ci list --repo alice/demo --state failed --limit 1").splitlines()]
        assert len(rows) == 1 and rows[0][1] == "failed" and rows[0][0] != r2, rows

    with subtest("a repository in manual mode: a push creates no run, a manual trigger does"):
        c4 = commit("", "manual mode")
        out = forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git -C /root/work push -q kajiba-git@127.0.0.1:alice/manual.git {c4}:refs/heads/main 2>&1")
        assert "kajiba: CI run" not in out, out
        manual_runs = "SELECT COUNT(*) FROM run JOIN repo ON repo.id = run.repo_id WHERE repo.config_key = 'manual'"
        time.sleep(3)
        assert sql(manual_runs) == "0"
        begin = time.monotonic()
        r5 = forge.succeed(f"{writer_cli} ci run --detach alice/manual {c4}").strip()
        assert time.monotonic() - begin < 10 and len(r5) == 26, r5
        assert "pass-ok" in forge.succeed(f"{writer_cli} ci watch {r5} 2>/dev/null")
        assert finished(r5) == "succeeded"
        assert sql(manual_runs) == "1" and sql(f"SELECT trigger FROM run WHERE public_id = '{r5}'") == "manual"

    with subtest("runners pass the relay, not the HTTP front; sources travel only as bundles"):
        # TLS is end to end: the front never saw kajiba.runner.v1.
        requests = [line.split() for line in forge.succeed("cat /var/log/nginx/kajiba.log").splitlines()]
        assert not [r for r in requests if r[1].startswith("/kajiba.runner.v1.")], requests
        # shortcut: bundle sizes are no longer measured here (the relay sees one TCP session for
        # all fetches); the runner's cache tests cover incremental bundles.
        lines = [line for line in unit_lines(forge, "kajiba-server.service") if line.get("event") == "request"]
        fetches = [line for line in lines if line.get("method") == "/kajiba.runner.v1.RunnerService/FetchRunBundle"]
        assert len(fetches) >= 3, fetches
        methods = {line.get("method") for line in lines}
        assert not {"git-info-refs", "git-upload-pack"} & methods, methods

    with subtest("admin retention deletes old logs and keeps their runs"):
        # ci.logRetention is 1m: the first run's log must be older.
        time.sleep(max(0, 65 - (time.monotonic() - r1_done)))
        runs = sql("SELECT COUNT(*) FROM run")
        first_attempt = attempts(r1)[0][0]
        forge.succeed(f"test -e {logs_dir}/{first_attempt}.log")
        out = forge.succeed("kajiba-server admin retention")
        assert out.startswith("logs deleted: ") and int(out.split()[2]) >= 1, out
        forge.fail(f"test -e {logs_dir}/{first_attempt}.log")
        assert sql(f"SELECT COUNT(*) FROM log_chunk WHERE attempt_id = (SELECT id FROM job_attempt WHERE public_id = '{first_attempt}')") == "0"
        assert sql("SELECT COUNT(*) FROM run") == runs
        assert sql(f"SELECT state FROM run WHERE public_id = '{r1}'") == "succeeded"
  '';
}
