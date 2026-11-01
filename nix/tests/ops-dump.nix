# J8 task 4: real hooks, an admitted import, runner recovery and verified online dumps.
{ pkgs, kajibaGitPackage, ... }@args:
let
  inherit (pkgs) lib;
  # Test-only latch AFTER successful PreReceive, before Git applies the refs.
  heldGit = kajibaGitPackage.overrideAttrs (old: {
    postPatch = (old.postPatch or "") + ''
      substituteInPlace apps/git/src/hook.rs --replace-fail \
        'if response.allowed {' \
        'if response.allowed {
            if std::path::Path::new("/var/lib/kajiba-git/hold-hook").exists() {
                std::fs::write("/var/lib/kajiba-git/hook-entered", "").unwrap();
                while std::path::Path::new("/var/lib/kajiba-git/hold-hook").exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }'
    '';
    doCheck = false;
  });
  heldCommands = pkgs.writeShellScriptBin "git" ''
    for arg in "$@"; do
      case "$arg" in
        fetch) latch=import ;;
        fsck) latch=copy ;;
      esac
    done
    if [ -n "''${latch:-}" ] && [ -e /var/lib/kajiba-fixtures/hold-$latch ]; then
      touch /var/lib/kajiba-fixtures/$latch-entered
      while [ -e /var/lib/kajiba-fixtures/hold-$latch ]; do sleep 0.02; done
    fi
    exec ${pkgs.git}/bin/git "$@"
  '';
in
import ./ci-common.nix (args // { kajibaGitPackage = heldGit; }) {
  name = "ops-dump";
  forgeConfig = {
    systemd.tmpfiles.rules = [ "d /var/lib/kajiba-fixtures 0777 root root -" ];
    systemd.services.kajiba-git.path = lib.mkBefore [ heldCommands ];
    systemd.services.kajiba-server.path = lib.mkBefore [ heldCommands ];
    services.kajiba-server.repos.imported = {
      owner = "alice";
      ci = "off";
    };
    # Shared test latches only; no production privilege change.
    systemd.services.kajiba-server.serviceConfig.ReadWritePaths = [ "/var/lib/kajiba-fixtures" ];
    systemd.services.kajiba-git.serviceConfig.ReadWritePaths = [ "/var/lib/kajiba-fixtures" ];
  };
  script = ''
    import time

    dump_cmd = "kajiba-server admin dump"
    series = "kajiba_dump_last_success_timestamp_seconds"

    def background(command, name):
        forge.succeed(f"(status=0; {command} >/tmp/{name}.out 2>/tmp/{name}.err || status=$?; echo $status >/tmp/{name}.status) </dev/null >/dev/null 2>&1 &")

    def done(name, expected=0):
        forge.wait_until_succeeds(f"test -e /tmp/{name}.status")
        assert int(forge.succeed(f"cat /tmp/{name}.status")) == expected, forge.succeed(f"cat /tmp/{name}.err")

    def maintenance():
        code, out = forge.execute("kajiba-server admin retention 2>&1")
        return code != 0 and "maintenance" in out.lower()

    with subtest("an admitted signed push and import drain while reads and runner jobs continue"):
        forge.succeed("chmod 777 /var/lib/kajiba-fixtures")
        long = '        long = check "long" "echo long-start; /tools/sleep 120; echo long-end; echo > $out";\n'
        initial = commit(long, "running before maintenance")
        run_id = push(initial)
        started(run_id)
        attempt = attempts(run_id)[0][0]
        job_unit = f"kajiba-job-{attempt}.service"
        job_pid = runner.succeed(f"systemctl show -P MainPID {job_unit}").strip()
        assert job_pid != "0"
        sha = commit("", "admitted across maintenance")
        forge.succeed("git clone --bare /root/work /var/lib/kajiba-fixtures/source.git", "chmod -R a+rX /var/lib/kajiba-fixtures/source.git")
        forge.succeed("touch /var/lib/kajiba-git/hold-hook /var/lib/kajiba-fixtures/hold-import /var/lib/kajiba-fixtures/hold-copy")
        push_cmd = f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git -C /root/work push kajiba-git@127.0.0.1:alice/demo.git {sha}:refs/heads/main"
        background(push_cmd, "push")
        forge.wait_until_succeeds("test -e /var/lib/kajiba-git/hook-entered")
        background("kajiba-server admin import alice/imported /var/lib/kajiba-fixtures/source.git", "import")
        forge.wait_until_succeeds("test -e /var/lib/kajiba-fixtures/import-entered")
        background(dump_cmd, "dump")
        for _ in range(100):
            if maintenance():
                break
            time.sleep(0.1)
        else:
            raise AssertionError("dump did not close admission")
        forge.succeed("test ! -e /tmp/dump.status")
        # Reconnect while drain is waiting, retaining the independent job unit.
        runner.succeed("systemctl restart kajiba-runner.service")
        runner.wait_for_unit("kajiba-runner.service")
        assert runner.succeed(f"systemctl show -P MainPID {job_unit}").strip() == job_pid
        forge.succeed("curl -fsS http://127.0.0.1:8080/healthz")
        forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git ls-remote kajiba-git@127.0.0.1:alice/demo.git")
        forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git clone kajiba-git@127.0.0.1:alice/demo.git /root/during-drain")
        assert forge.succeed("git -C /root/during-drain rev-parse HEAD").strip() == initial
        for command in [dump_cmd, "kajiba-server admin import alice/imported /var/lib/kajiba-fixtures/source.git", f"{writer_cli} ci run --detach alice/demo {initial}", push_cmd.replace("refs/heads/main", "refs/heads/refused")]:
            code, out = forge.execute(command + " 2>&1")
            assert code != 0 and "maintenance" in out.lower(), out
        # The client-streaming upload is refused at admission, before reading a bundle.
        token = forge.succeed("tail -n1 /root/writer-token").strip()
        code, out = forge.execute(f"grpcurl -plaintext -import-path {proto} -proto kajiba/v1/run.proto -H 'authorization: Bearer {token}' -d '{{}}' 127.0.0.1:8080 kajiba.v1.RunService/CreateWorktreeRun 2>&1")
        assert code != 0 and has_reason(out, "MAINTENANCE"), out
        forge.succeed("rm /var/lib/kajiba-git/hold-hook /var/lib/kajiba-fixtures/hold-import")
        done("push")
        done("import")
        forge.wait_until_succeeds("test -e /var/lib/kajiba-fixtures/copy-entered")
        assert maintenance()
        forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git ls-remote kajiba-git@127.0.0.1:alice/demo.git")
        assert metric(forge, metrics_url, series) == 0
        forge.succeed("rm /var/lib/kajiba-fixtures/hold-copy")
        done("dump")
        directory = forge.succeed("cat /tmp/dump.out").strip()
        assert directory.startswith("/var/lib/kajiba-dumps/dump-"), directory
        snapshot = directory + "/server/db/kajiba.sqlite"
        def saved(query):
            return forge.succeed(f"sqlite3 {snapshot} {shlex.quote(query)}").strip()
        assert saved("SELECT count(*) FROM pending_push") == "0"
        # One event for each applied ref (push and import), no duplicate from hooks/recovery.
        query = f"SELECT count(*) FROM event WHERE kind = 'push' AND payload LIKE '%{sha}%'"
        assert saved(query) == "2", saved(query)
        assert saved("SELECT count(*) FROM run WHERE sha = '" + sha + "' AND trigger = 'push'") == "1"
        assert saved("SELECT last_event_id FROM handler_checkpoint WHERE handler = 'ci'") == saved("SELECT max(id) FROM event")
        for config_key in ["demo", "imported"]:
            public_id = saved(f"SELECT public_id FROM repo WHERE config_key = '{config_key}'")
            assert forge.succeed(f"runuser -u kajiba-server -- git --git-dir={directory}/git/repos/{public_id}.git rev-parse main").strip() == sha
        assert metric(forge, metrics_url, series) > 0
        runner.succeed(f"systemctl is-active {job_unit}")
        assert not [e for e in unit_lines(forge, "kajiba-server.service") if e.get("event") == "dump-failed"]
        events = [e for e in unit_lines(forge, "kajiba-server.service") if e.get("event") == "dump-done"]
        assert len(events) == 1 and events[0]["dir"] == directory and events[0]["dur_us"] > 0, events
        forge.succeed("kajiba-server admin retention")
        assert finished(run_id) == "succeeded"
        assert attempts(run_id) == [(attempt, "1", "succeeded")]

    with subtest("copy failure releases maintenance, preserves success metric and publishes nothing"):
        before = metric(forge, metrics_url, series)
        inventory = forge.succeed("find /var/lib/kajiba-dumps -maxdepth 1 -mindepth 1 | sort")
        forge.succeed("mkdir -p /var/lib/kajiba-git/tmp/orphans; ln -s missing /var/lib/kajiba-git/tmp/orphans/unsafe")
        code, out = forge.execute(dump_cmd + " 2>&1")
        assert code != 0, out
        assert metric(forge, metrics_url, series) == before
        assert forge.succeed("find /var/lib/kajiba-dumps -maxdepth 1 -mindepth 1 | sort") == inventory
        forge.succeed("kajiba-server admin retention", "rm /var/lib/kajiba-git/tmp/orphans/unsafe")
        resumed = commit("", "writes resume after failed dump")
        assert finished(push(resumed)) == "succeeded"
        forge.succeed(dump_cmd)
        assert len([e for e in unit_lines(forge, "kajiba-server.service") if e.get("event") == "dump-done"]) == 2
        forge.fail("runuser -u kajiba-server -- touch /var/lib/kajiba-git/repos/forbidden")
  '';
}
