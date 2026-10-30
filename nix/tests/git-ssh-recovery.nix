# J3 task 29: real SSH pushes interrupted at deterministic hook boundaries.
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
let
  inherit (pkgs) lib;
  control = "/var/lib/kajiba-git/recovery-test";
  # Only this VM replaces the global hooks: admission still uses the real binary.
  barrier = pkgs.writeText "kajiba-recovery-barrier" ''
    barrier() {
      if test -e ${control}/hold-"$1"; then
        echo "$PPID" > ${control}/child
        echo "$KAJIBA_SESSION" > ${control}/session
        touch ${control}/entered-"$1"
        while ! test -e ${control}/release-"$1"; do sleep 0.05; done
      fi
    }
  '';
  hooks = pkgs.runCommand "kajiba-recovery-hooks" { } ''
    mkdir "$out"
    cat > "$out/pre-receive" <<'HOOK'
    #!${pkgs.runtimeShell}
    . ${barrier}
    ${lib.getExe kajibaGitPackage} hook pre-receive || exit $?
    barrier pre
    HOOK
    cat > "$out/post-receive" <<'HOOK'
    #!${pkgs.runtimeShell}
    . ${barrier}
    barrier post-before
    ${lib.getExe kajibaGitPackage} hook post-receive
    status=$?
    echo "$status" > ${control}/post-status
    barrier post-after
    exit "$status"
    HOOK
    cat > "$out/update" <<'HOOK'
    #!${pkgs.runtimeShell}
    if test -e ${control}/fail-updates; then
      case "$1" in refs/heads/failed*) exit 1 ;; esac
    fi
    HOOK
    chmod +x "$out/"*
  '';
  gitConfig = (pkgs.formats.toml { }).generate "kajiba-recovery-test.toml" {
    hooks_path = toString hooks;
    metrics.listen = "127.0.0.1:9433";
  };
in
pkgs.testers.runNixOSTest {
  name = "kajiba-git-ssh-recovery";

  nodes.forge = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git = {
      package = kajibaGitPackage;
      metrics.listen = "127.0.0.1:9433";
    };
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      http.publicUrl = "https://forge.example.org";
      metrics.listen = "127.0.0.1:9431";
      users.alice = {
        sshKeys = [ (builtins.readFile ./fixtures/ssh-writer-key.pub) ];
        signingKeys = [ (builtins.readFile ./fixtures/ssh-signing-key.pub) ];
      };
      repos.demo = {
        owner = "alice";
        description = "SSH crash recovery";
      };
    };
    # Keep each crash observable until the test explicitly starts the next generation.
    systemd.services.kajiba-server = {
      unitConfig.StartLimitIntervalSec = 0;
      serviceConfig.Restart = lib.mkForce "no";
    };
    systemd.services.kajiba-git = {
      unitConfig.StartLimitIntervalSec = 0;
      serviceConfig = {
        ExecStart = lib.mkForce "${lib.getExe kajibaGitPackage} serve --config ${gitConfig}";
        Restart = lib.mkForce "no";
      };
    };
    environment.systemPackages = [
      pkgs.git
      pkgs.openssh
      pkgs.sqlite
      pkgs.curl
      pkgs.procps
    ];
  };

  testScript = testLib + ''
    import json

    unit = "kajiba-server.service"
    git_unit = "kajiba-git.service"
    db = "/var/lib/kajiba-server/db/kajiba.sqlite"
    control = "${control}"
    metrics_url = "http://127.0.0.1:9431/metrics"
    git_metrics_url = "http://127.0.0.1:9433/metrics"
    zero = "0" * 40
    ssh = "ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/known_hosts -o ConnectTimeout=5 -i /root/auth-key -p 2222"

    def sql(query):
        return forge.succeed(f"sqlite3 -cmd '.timeout 5000' {db} {shlex.quote(query)}").strip()

    def rows(query):
        out = forge.succeed(f"sqlite3 -cmd '.timeout 5000' -json {db} {shlex.quote(query)}")
        return json.loads(out or "[]")

    def events():
        return rows("SELECT * FROM event WHERE kind='push' ORDER BY id")

    def pending():
        return rows("SELECT * FROM pending_push ORDER BY id")

    def remote_refs():
        return refs(forge, directory, user="kajiba-git")

    def journal(service):
        forge.succeed("journalctl --sync")
        return [json.loads(line) for line in forge.succeed(f"journalctl _SYSTEMD_UNIT={service} -o cat --no-pager").splitlines()]

    def started():
        return [line for line in journal(git_unit) if line.get("event") == "session-started"]

    def generation():
        return [line["generation"] for line in journal(unit) if line.get("event") == "git-side-connected"][-1]

    def metric(url, series):
        lines = forge.succeed(f"curl -fsS {url}").splitlines()
        return float(next(line.split()[-1] for line in lines if line.startswith(series + " ")))

    def idle():
        series = 'kajiba_git_sessions_active{kind="receive_pack"} 0'
        forge.wait_until_succeeds(f"curl -fsS {git_metrics_url} | grep -Fx {shlex.quote(series)}")

    def settled():
        forge.wait_until_succeeds(f"test $(sqlite3 {db} 'SELECT COUNT(*) FROM pending_push') = 0")
        idle()

    def ready():
        forge.wait_until_succeeds("curl -fsS http://127.0.0.1:8080/readyz")

    def commit(message):
        forge.succeed(f"git -C /root/work commit --allow-empty -S -m {shlex.quote(message)}")
        return forge.succeed("git -C /root/work rev-parse HEAD").strip()

    def push_command(specs, transport=ssh):
        return f"timeout 120 env GIT_SSH_COMMAND={shlex.quote(transport)} git -C /root/work push kajiba-git@127.0.0.1:alice/demo.git {specs}"

    def push(specs, reason=None, transport=ssh):
        code, out = forge.execute(push_command(specs, transport) + " 2>&1")
        if reason is None:
            assert code == 0, out
            settled()
        else:
            assert code != 0 and code != 124 and reason in out, (code, out)
        return out

    def hold(specs, stages):
        settled()
        forge.succeed(f"rm -f {control}/hold-* {control}/entered-* {control}/release-* {control}/post-status /root/push.status")
        for stage in stages:
            forge.succeed(f"touch {control}/hold-{stage}")
        before = events()
        # The test driver's errexit must not skip recording an expected push failure.
        forge.succeed(f"(status=0; {push_command(specs)} || status=$?; echo $status > /root/push.status) > /root/push.out 2>&1 < /dev/null &")
        forge.wait_for_file(f"{control}/entered-{stages[0]}")
        row, = pending()
        assert row["git_generation"] == generation()
        assert row["git_session_id"] == forge.succeed(f"cat {control}/session").strip()
        assert events() == before
        assert_child()
        return before, row

    def assert_child():
        child = forge.succeed(f"cat {control}/child").strip()
        forge.succeed(f"kill -0 {child}")
        assert "receive-pack" in forge.succeed(f"ps -o args= -p {child}")
        assert forge.succeed(f"ps -o ppid= -p {child}").strip() == forge.succeed(f"systemctl show -P MainPID {git_unit}").strip()
        return child

    def release(stage):
        forge.succeed(f"touch {control}/release-{stage}")

    def finished(success=True):
        forge.wait_until_succeeds("test -s /root/push.status", timeout=30)
        status = int(forge.succeed("cat /root/push.status"))
        out = forge.succeed("cat /root/push.out")
        assert status != 124 and (status == 0) == success, (status, out)
        forge.succeed(f"rm -f {control}/hold-*")
        return out

    def crash(service):
        forge.succeed(f"systemctl kill --signal=KILL --kill-whom=main {service}")
        forge.wait_until_succeeds(f"systemctl is-failed {service}")
        forge.wait_until_succeeds(f"test $(systemctl show -P MainPID {service}) = 0")

    def converted(before, row, applied):
        settled()
        after = events()
        assert after[:len(before)] == before
        added = after[len(before):]
        assert [json.loads(event["payload"]) for event in added] == applied, added
        for event in added:
            assert event["version"] == 1 and event["principal_kind"] == "user"
            assert event["principal_id"] == row["principal_id"] == int(sql("SELECT id FROM user WHERE name='alice'"))
            assert event["repo_id"] == row["repo_id"]
            assert event["correlation_id"] == row["correlation_id"]
        return after

    def replay():
        before = events()
        forge.succeed(f"systemctl restart {unit}")
        ready()
        settled()
        assert events() == before

    forge.wait_for_unit(unit)
    forge.wait_for_unit(git_unit)
    forge.wait_for_unit("kajiba-sshd.service")
    ready()
    directory = "/var/lib/kajiba-git/repos/" + sql("SELECT public_id FROM repo WHERE config_key='demo'") + ".git"
    forge.succeed(
        f"install -d -o kajiba-git -g kajiba-git {control}",
        "install -m 600 ${./fixtures/ssh-writer-key} /root/auth-key",
        "install -m 600 ${./fixtures/ssh-signing-key} /root/signing-key",
        "ssh-keyscan -p 2222 127.0.0.1 > /root/known_hosts",
        "git init -b main /root/work",
        "git -C /root/work config user.name Alice",
        "git -C /root/work config user.email alice@example.org",
        "git -C /root/work config gpg.format ssh",
        "git -C /root/work config user.signingkey /root/signing-key",
    )
    initial = commit("initial")
    push("HEAD:refs/heads/main")
    assert len(events()) == 1

    with subtest("server stopped refuses a push before starting any child or changing durable state"):
        # Authenticate first so AuthorizedKeysCommand cannot mask OpenSession's refusal.
        forge.succeed(f"{ssh} -M -N -f -S /root/ssh-control kajiba-git@127.0.0.1")
        before_refs, before_pending, before_events = remote_refs(), pending(), events()
        before_started = started()
        forge.succeed(f"systemctl stop {unit}")
        push("HEAD:refs/heads/offline", reason="server unavailable", transport=ssh + " -S /root/ssh-control")
        assert remote_refs() == before_refs and pending() == before_pending and events() == before_events
        assert started() == before_started
        idle()
        forge.fail("pgrep -u kajiba-git -f 'git .*receive-pack'")
        forge.succeed(f"{ssh} -S /root/ssh-control -O exit kajiba-git@127.0.0.1", f"systemctl start {unit}")
        ready()

    with subtest("server crash before PostReceive preserves a live session and orders the next push after recovery"):
        first = commit("server interrupted")
        before, row = hold("HEAD:refs/heads/main", ["post-before", "post-after"])
        assert remote_refs()["refs/heads/main"] == first
        daemon_pid = forge.succeed(f"systemctl show -P MainPID {git_unit}")
        crash(unit)
        release("post-before")
        forge.wait_for_file(f"{control}/entered-post-after")
        assert int(forge.succeed(f"cat {control}/post-status")) != 0
        forge.wait_until_succeeds("grep -F 'server unavailable' /root/push.out")
        assert pending() == [row] and events() == before
        forge.succeed(f"systemctl start {unit}")
        ready()
        assert forge.succeed(f"systemctl show -P MainPID {git_unit}") == daemon_pid
        assert generation() == row["git_generation"]
        assert_child()
        assert pending() == [row] and events() == before
        second = commit("after server recovery")
        push("HEAD:refs/heads/main", reason="unfinished session")
        assert remote_refs()["refs/heads/main"] == first
        assert pending() == [row] and events() == before
        release("post-after")
        finished()
        first_events = converted(before, row, [{"ref": "refs/heads/main", "old": initial, "new": first}])
        assert any(line.get("event") == "session-exited" and line.get("session") == row["git_session_id"] for line in journal(git_unit))
        push("HEAD:refs/heads/main")
        assert events()[:-1] == first_events
        assert json.loads(events()[-1]["payload"]) == {"ref": "refs/heads/main", "old": first, "new": second}
        assert events()[-1]["id"] > first_events[-1]["id"]
        replay()

    with subtest("SessionExited while the server is down is recovered exactly once on its return"):
        tip = commit("exit during server outage")
        old = remote_refs()["refs/heads/main"]
        before, row = hold("HEAD:refs/heads/main", ["post-before"])
        crash(unit)
        release("post-before")
        finished()
        idle()
        assert pending() == [row] and events() == before
        assert any(line.get("event") == "session-exited" and line.get("session") == row["git_session_id"] for line in journal(git_unit))
        forge.succeed(f"systemctl start {unit}")
        ready()
        converted(before, row, [{"ref": "refs/heads/main", "old": old, "new": tip}])
        replay()

    with subtest("daemon death after admission but before any ref change emits no event in the new generation"):
        tip = commit("daemon interrupted before refs")
        before_refs = remote_refs()
        before, row = hold("HEAD:refs/heads/main", ["pre"])
        assert remote_refs() == before_refs
        child = assert_child()
        crash(git_unit)
        out = finished(success=False)
        assert "git daemon disconnected before session exit" in out, out
        forge.fail(f"kill -0 {child}")
        assert remote_refs() == before_refs and pending() == [row] and events() == before
        forge.succeed(f"systemctl start {git_unit}")
        ready()
        converted(before, row, [])
        assert generation() != row["git_generation"]
        replay()
        push("HEAD:refs/heads/main")
        assert len(events()) == len(before) + 1
        assert json.loads(events()[-1]["payload"]) == {"ref": "refs/heads/main", "old": before_refs["refs/heads/main"], "new": tip}

    with subtest("daemon death after partial success recovers creation, deletion and update in recorded order"):
        push("HEAD:refs/heads/deleted HEAD:refs/heads/failed HEAD:refs/heads/failed-delete")
        before_refs = remote_refs()
        tip = commit("partial ref updates")
        forge.succeed(f"touch {control}/fail-updates")
        specs = "HEAD:refs/heads/main HEAD:refs/heads/created :refs/heads/deleted HEAD:refs/heads/failed HEAD:refs/heads/failed-create :refs/heads/failed-delete"
        before, row = hold(specs, ["post-before"])
        updates = json.loads(row["updates"])
        assert len(updates) == 6
        applied = [update for update in updates if not update["ref"].startswith("refs/heads/failed")]
        assert {update["ref"] for update in applied} == {"refs/heads/main", "refs/heads/created", "refs/heads/deleted"}
        assert next(update for update in applied if update["ref"] == "refs/heads/created")["old"] == zero
        assert next(update for update in applied if update["ref"] == "refs/heads/deleted")["new"] == zero
        expected = dict(before_refs)
        expected.update({"refs/heads/main": tip, "refs/heads/created": tip})
        del expected["refs/heads/deleted"]
        assert remote_refs() == expected
        child = assert_child()
        crash(git_unit)
        out = finished(success=False)
        assert "git daemon disconnected before session exit" in out, out
        forge.fail(f"kill -0 {child}")
        assert pending() == [row] and events() == before and remote_refs() == expected
        forge.succeed(f"rm {control}/fail-updates", f"systemctl start {git_unit}")
        ready()
        converted(before, row, applied)
        assert generation() != row["git_generation"]
        assert remote_refs() == expected
        replay()
        # A second generation and startup pass must not replay the applied subset.
        saved = events()
        previous = generation()
        forge.succeed(f"systemctl restart {git_unit}")
        # readyz stays OK across a fast restart; admission needs the new generation's snapshot.
        retry(lambda _: generation() != previous)
        ready()
        settled()
        assert events() == saved
        commit("after partial recovery")
        push("HEAD:refs/heads/main")
        assert events()[:-1] == saved

    with subtest("an unexpected ref retains the entire row across restarts and blocks admission"):
        tip = commit("unexpected ref evidence")
        before, row = hold("HEAD:refs/heads/main HEAD:refs/heads/evidence", ["post-before"])
        # Deliberate out-of-band corruption, outside ADR 0003's durability guarantee.
        forge.succeed(f"runuser -u kajiba-git -- git --git-dir={directory} update-ref refs/heads/main {initial} {tip}")
        release("post-before")
        finished()
        idle()
        forge.wait_until_succeeds(f"test $(sqlite3 {db} 'SELECT COUNT(*) FROM pending_push WHERE git_session_id IS NULL') = 1")
        retained = dict(row, git_session_id=None)
        assert pending() == [retained] and events() == before
        before_refs = remote_refs()
        assert before_refs["refs/heads/evidence"] == tip
        for restart in [False, True]:
            if restart:
                forge.succeed(f"systemctl restart {unit}")
                ready()
            push("HEAD:refs/heads/blocked", reason="unfinished session")
            assert pending() == [retained] and events() == before and remote_refs() == before_refs
        assert any(line.get("event") == "push-retained" for line in journal(unit))
        # Repair only the fixture; startup uses the same ordered conversion to unblock it.
        forge.succeed(f"runuser -u kajiba-git -- git --git-dir={directory} update-ref refs/heads/main {tip} {initial}", f"systemctl restart {unit}")
        ready()
        converted(before, row, json.loads(row["updates"]))
        replay()
        commit("after evidence recovery")
        push("HEAD:refs/heads/main")

    with subtest("recovered event logs are unique and both live metrics endpoints validate"):
        settled()
        ready()
        pushes = [line for line in journal(unit) if line.get("event") == "push"]
        assert len(pushes) == len(events())
        for log, event in zip(pushes, events()):
            assert log["request_id"] == event["correlation_id"]
            assert log["repo"] == "alice/demo" and log["user"] == "alice"
            for field, value in json.loads(event["payload"]).items():
                assert log[field] == value
        assert metric(metrics_url, 'kajiba_pushes_total{result="accepted"}') > 0
        assert metric(git_metrics_url, 'kajiba_git_sessions_total{kind="receive_pack",result="ok"}') > 0
        validate_logs(forge, unit)
        validate_logs(forge, git_unit)
        validate_metrics(forge, metrics_url)
        validate_metrics(forge, git_metrics_url)
  '';
}
