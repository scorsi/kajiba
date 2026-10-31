# J6 task 20: recovery around a long job. A WatchRun stream outlives the request timeout with
# keepalives (0129), a server restart loses or doubles nothing, a frozen runner loses its lease
# and the job is replayed, its late result ignored (0105); a restarted runner process loses its
# attempt at reconnection and the job is replayed (0008).
{
  kajibaServerPackage,
  kajibaRunnerPackage,
  ...
}@args:
let
  # Lease 30 s → 10 s, heartbeat 10 s → 3 s, request timeout 30 s → 10 s, keepalive 20 s → 5 s:
  # this test only, production is unchanged; --replace-fail breaks the build if a constant is
  # renamed. Unit tests run on the production builds, not on these.
  shortened =
    package: substitutions:
    package.overrideAttrs (old: {
      postPatch = (old.postPatch or "") + substitutions;
      doCheck = false;
    });
  serverPackage = shortened kajibaServerPackage ''
    substituteInPlace apps/server/src/db/attempts.rs \
      --replace-fail 'pub const LEASE_MS: i64 = 30_000;' 'pub const LEASE_MS: i64 = 10_000;'
    substituteInPlace apps/server/src/transport.rs \
      --replace-fail '    request: Duration::from_secs(30),' '    request: Duration::from_secs(10),'
    substituteInPlace apps/server/src/transport/runs.rs \
      --replace-fail 'const WATCH_KEEPALIVE: Duration = Duration::from_secs(20);' \
                     'const WATCH_KEEPALIVE: Duration = Duration::from_secs(5);'
  '';
  runnerPackage = shortened kajibaRunnerPackage ''
    substituteInPlace apps/runner/src/session.rs \
      --replace-fail 'pub const HEARTBEAT: Duration = Duration::from_secs(10);' \
                     'pub const HEARTBEAT: Duration = Duration::from_secs(3);'
  '';
in
import ./ci-common.nix args {
  name = "ci-recovery";
  inherit serverPackage runnerPackage;
  script = ''
    # Outlasts the 10 s request timeout and lease, and several 5 s keepalive periods.
    LONG = '        long = check "long" "echo long-start; /tools/sleep 20; echo long-end; echo > $out";\n'

    with subtest("a WatchRun stream outlives the 10 s request timeout with keepalives; status says 75 meanwhile"):
        c4 = commit(LONG, "fourth")
        r4 = push(c4)
        before = len(watch_requests())
        forge.succeed(f"(set +e; begin=$(date +%s); {writer_cli} ci watch {r4} >/tmp/r4.out 2>/tmp/r4.err; echo $? $(( $(date +%s) - begin )) >/tmp/r4.status) >/dev/null 2>&1 &")
        # The raw stream, to see the keepalives the CLI ignores.
        request = shlex.quote(json.dumps({"run": r4, "follow": True}))
        forge.succeed(f"(grpcurl -plaintext -H \"authorization: Bearer $(sed -n 2p /root/writer-token)\" -import-path {proto} -proto kajiba/v1/run.proto -d {request} 127.0.0.1:8080 kajiba.v1.RunService/WatchRun >/tmp/r4.grpc 2>&1; echo $? >/tmp/r4.grpc-status) </dev/null >/dev/null 2>&1 &")
        started(r4)
        code, _ = forge.execute(f"{writer_cli} ci status {r4} 2>/dev/null")
        assert code == 75, code
        forge.wait_until_succeeds("test -s /tmp/r4.status", timeout=300)
        status, seconds = forge.succeed("cat /tmp/r4.status").split()
        assert status == "0" and int(seconds) > 10, (status, seconds)
        out = forge.succeed("cat /tmp/r4.out")
        assert "long-start" in out and "long-end" in out, out
        forge.wait_until_succeeds("test -s /tmp/r4.grpc-status", timeout=60)
        out = forge.succeed("cat /tmp/r4.grpc")
        assert forge.succeed("cat /tmp/r4.grpc-status").strip() == "0" and '"keepalive"' in out and '"finished"' in out, out
        new = watch_requests()[before:]
        # The CLI hangs up once it has the finished run, before the trailers: CANCELLED, never DEADLINE_EXCEEDED.
        assert len(new) == 2 and all(r.get("grpc_code") in ("OK", "CANCELLED") and r["dur_us"] > 10000000 for r in new), new

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
        # A stale runner is never matched (it is the only one), so the replay waits for it to wake;
        # the old attempt then reports late and is ignored.
        runner.succeed(f"kill -CONT {pid}")
        forge.wait_until_succeeds(f"test $(sqlite3 {db} \"SELECT COUNT(*) FROM job_attempt a JOIN job ON job.id = a.job_id JOIN run ON run.id = job.run_id WHERE run.public_id = '{r6}'\") -ge 2")
        assert finished(r6) == "succeeded"
        runner.wait_until_succeeds(f"journalctl _SYSTEMD_UNIT=kajiba-runner.service -o cat | grep attempt-finished | grep -F {first}")
        tries = attempts(r6)
        assert tries[0] == (first, "1", "lost") and tries[-1][2] == "succeeded", tries
        kinds = sql(f"SELECT kind FROM event WHERE id > {mark}").splitlines()
        assert "attempt-lost" in kinds and "attempt-replayed" in kinds, kinds
        assert finishes(r6) == 1

    with subtest("a restarted runner process: its hello lists nothing, the attempt is lost and the job replayed"):
        c7 = commit(LONG, "seventh")
        r7 = push(c7)
        started(r7)
        first = attempts(r7)[0][0]
        runner.succeed("systemctl restart kajiba-runner.service")
        # Lost on reconnection: the new process's heartbeats must not keep it alive.
        forge.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT state FROM job_attempt WHERE public_id = '{first}'\")\" = lost", timeout=60)
        assert finished(r7) == "succeeded"
        tries = attempts(r7)
        assert tries[0] == (first, "1", "lost") and len(tries) == 2 and tries[1][2] == "succeeded", tries
        assert finishes(r7) == 1
  '';
}
