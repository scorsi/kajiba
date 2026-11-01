# J1 task 28: the server becomes ready through the module, reconciles a declared user and key,
# keeps its rows across a reboot, re-migrates a wiped stateDir, and refuses a second instance
# on the same stateDir (0012, 0014, 0015, 0023); it serves the web application by default (0094, 0128).
# A switch that declares a repository reloads the unit instead of restarting it, and a restart is
# prompt (0140, 0141).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  kajibaUiPackage,
  testLib,
}:
let
  inherit (pkgs) lib;
  stateDir = "/var/lib/kajiba-server";
  keyBlob = lib.elemAt (lib.splitString " " (builtins.readFile ./fixtures/ssh-admin-key.pub)) 1;
  # Hold an admitted receive-pack before exec, so the restart overlaps a real session.
  drainingGit = pkgs.writeShellScriptBin "git" ''
    for arg in "$@"; do
      if [ "$arg" = receive-pack ] && [ -e /var/lib/kajiba-git/hold-push ]; then
        touch /var/lib/kajiba-git/push-started
        while [ -e /var/lib/kajiba-git/hold-push ]; do sleep 0.05; done
      fi
    done
    exec ${pkgs.git}/bin/git "$@"
  '';
  # Every migration shipped with the binary: a fresh stateDir applies all of them.
  migrations = map (file: lib.toInt (lib.head (lib.splitString "_" file))) (
    lib.attrNames (builtins.readDir ../../apps/server/migrations)
  );
in
pkgs.testers.runNixOSTest {
  name = "kajiba-server-lifecycle";

  nodes.machine = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git.package = kajibaGitPackage;
    systemd.services.kajiba-git.path = lib.mkBefore [ drainingGit ];
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      ui.package = kajibaUiPackage;
      inherit stateDir;
      http.publicUrl = "https://forge.example.org";
      users.alice = {
        isAdmin = true;
        signingKeys = [ (builtins.readFile ./fixtures/ssh-admin-key.pub) ];
        sshKeys = [ "ssh-ed25519 ${keyBlob} alice@example.org" ];
      };
    };
    environment.systemPackages = [
      pkgs.curl
      pkgs.sqlite
      pkgs.git
      pkgs.openssh
    ];
    # Only a reloadable setting differs: switching to it reloads the server (0141).
    specialisation.repository.configuration.services.kajiba-server.repos.demo = {
      owner = "alice";
      description = "";
    };
  };

  testScript = testLib + ''
    import json

    unit = "kajiba-server.service"
    db = "${stateDir}/db/kajiba.sqlite"
    migrations = {${lib.concatMapStringsSep ", " toString migrations}}


    def sql(query):
        # Root reads while the service runs, so the WAL files already exist and stay owned by kajiba-server.
        return machine.succeed(f"sqlite3 {db} {shlex.quote(query)}").strip()


    def identity():
        return sql(
            "SELECT u.public_id, u.name, u.is_admin, u.source, u.disabled_at IS NULL,"
            " k.public_id, k.usage, k.key_type, instr(k.public_key, '${keyBlob}') > 0, k.source"
            " FROM user u JOIN ssh_key k ON k.user_id = u.id WHERE k.usage = 'auth'"
        ).split("|")


    def without_ids(row):
        return row[1:5] + row[6:]


    def invocation_events():
        machine.succeed("journalctl --sync")
        invocation = machine.succeed(f"systemctl show -P InvocationID {unit}").strip()
        out = machine.succeed(f"journalctl _SYSTEMD_INVOCATION_ID={invocation} _SYSTEMD_UNIT={unit} -o cat --no-pager")
        return [json.loads(line) for line in out.splitlines()]


    def applied(events):
        return {event["version"] for event in events if event["event"] == "migration-applied"}


    def assert_ready():
        machine.wait_for_unit(unit)
        # systemd reaches active only once ExecStartPost exited 0; its curl is the /readyz 200
        # request logged by this invocation (systemctl show keeps no ExecStartPost status).
        state = machine.succeed(f"systemctl show -P ActiveState {unit}").strip()
        assert state == "active", f"ActiveState: {state!r}"
        events = invocation_events()
        probes = [e for e in events if e["event"] == "request" and e.get("method") == "readyz" and e.get("status") == 200]
        assert probes, f"no /readyz 200 request in this invocation: {events}"
        machine.succeed("curl -fsS --max-time 2 http://127.0.0.1:8080/healthz")
        # A fresh SSH banner probe can time out under VM load after startup's success.
        machine.wait_until_succeeds("curl -fsS --max-time 2 http://127.0.0.1:8080/readyz", timeout=30)


    with subtest("ready through the module after migrating a fresh stateDir"):
        assert_ready()
        events = invocation_events()
        assert applied(events) == migrations, f"applied {applied(events)} != {migrations}: {events}"
        assert "ready" in {event["event"] for event in events}, f"no ready line: {events}"

    with subtest("the server itself serves the web application, with the SPA fallback"):
        for path in ["/", "/-/login"]:
            out = machine.succeed(f"curl -sS --max-time 5 -o /tmp/page.html -w '%{{http_code}} %{{content_type}}' http://127.0.0.1:8080{path}")
            code, content_type = out.split(" ", 1)
            assert code == "200" and content_type.startswith("text/html"), f"GET {path}: {out!r}"
            machine.succeed("grep -qi '<!doctype html>' /tmp/page.html")

    with subtest("both units use the split identities, directories and socket ownership"):
        machine.wait_for_unit("kajiba-git.service")
        paths = {
            "${stateDir}": "kajiba-server:kajiba-server 750",
            "/var/lib/kajiba-git": "kajiba-git:kajiba-git 750",
            "/run/kajiba": "kajiba-server:kajiba-server 751",
            "/run/kajiba-git": "kajiba-git:kajiba-git 750",
            "/run/kajiba/local.sock": "kajiba-server:kajiba-server 600",
            "/run/kajiba/git.sock": "kajiba-server:kajiba-git 660",
            "/run/kajiba-git/local.sock": "kajiba-git:kajiba-git 660",
            "/run/kajiba-git/session.sock": "kajiba-git:kajiba-git 600",
        }
        for path, expected in paths.items():
            assert machine.succeed(f"stat -c '%U:%G %a' {path}").strip() == expected, path
        assert machine.succeed("id -Gn kajiba-server").split() == ["kajiba-server", "kajiba-git"]
        assert machine.succeed("id -Gn kajiba-git").split() == ["kajiba-git"]
        machine.fail("runuser -u kajiba-server -- touch /var/lib/kajiba-git/repos/forbidden")
        machine.fail(f"runuser -u kajiba-git -- cat {db}")
        # Exercise the server's mount namespace as root too: ReadOnlyPaths is independent of modes.
        pid = machine.succeed(f"systemctl show -P MainPID {unit}").strip()
        machine.fail(f"nsenter -t {pid} -m -- touch /var/lib/kajiba-git/repos/forbidden")
        machine.succeed("runuser -u kajiba-git -- kajiba-git config check /etc/kajiba/git.toml")

    with subtest("SSH readiness fails and recovers without restarting either daemon"):
        machine.wait_for_unit("kajiba-sshd.service")
        invocations = {
            service: machine.succeed(f"systemctl show -P InvocationID {service}").strip()
            for service in [unit, "kajiba-git.service"]
        }
        machine.succeed("systemctl stop kajiba-sshd.service")
        machine.wait_until_succeeds("test $(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/readyz) = 503")
        machine.succeed("curl -fsS http://127.0.0.1:8080/healthz")
        machine.succeed("systemctl start kajiba-sshd.service")
        machine.wait_until_succeeds("curl -fsS http://127.0.0.1:8080/readyz")
        for service, invocation in invocations.items():
            assert machine.succeed(f"systemctl show -P InvocationID {service}").strip() == invocation

    with subtest("resource limits apply to the running unit"):
        assert machine.succeed(f"systemctl show -P TasksMax {unit}").strip() == "1024"
        assert machine.succeed(f"systemctl show -P MemoryMax {unit}").strip() == str(2 * 1024**3)
        pid = machine.succeed(f"systemctl show -P MainPID {unit}").strip()
        limits = {
            line[:26].strip(): line[26:].split()[:2]
            for line in machine.succeed(f"cat /proc/{pid}/limits").splitlines()[1:]
        }
        assert limits["Max core file size"] == ["0", "0"], f"limits: {limits}"
        assert limits["Max open files"] == ["8192", "8192"], f"limits: {limits}"

    with subtest("declared user and SSH key reconciled into the database"):
        first = identity()
        assert without_ids(first) == ["alice", "1", "config", "1", "auth", "ssh-ed25519", "1", "config"], f"identity row: {first}"

    with subtest("a second serve on the same stateDir is refused by kajiba.lock"):
        machine.fail("runuser -u kajiba-server -- kajiba-server serve --config /etc/kajiba/server.toml 2>/tmp/second.log")
        lines = [json.loads(line) for line in machine.succeed("cat /tmp/second.log").splitlines()]
        fatal = [line for line in lines if line["event"] == "fatal"]
        assert len(fatal) == 1 and "another instance holds ${stateDir}/kajiba.lock" in fatal[0]["err"], f"second serve output: {lines}"
        validate_log_output(machine, "cat /tmp/second.log")
        assert_ready()

    with subtest("rows survive a reboot with the persisted stateDir, without re-migration"):
        machine.shutdown()
        machine.start()
        assert_ready()
        events = invocation_events()
        assert applied(events) == set(), f"re-migrated {applied(events)}: {events}"
        after = identity()
        assert after == first, f"{after} != {first}"

    with subtest("a wiped stateDir is migrated again from scratch"):
        machine.succeed(f"systemctl stop {unit}", "rm -rf ${stateDir}", f"systemctl start {unit}")
        assert_ready()
        events = invocation_events()
        assert applied(events) == migrations, f"applied {applied(events)} != {migrations}: {events}"
        fresh = identity()
        # Same declared content, new rows: the reconciliation re-created them from the config.
        assert without_ids(fresh) == without_ids(first), f"{fresh} != {first}"
        assert fresh[0] != first[0] and fresh[5] != first[5], f"public ids kept: {fresh} vs {first}"

    with subtest("declaring a repository reloads the server: same process, repository created"):
        pid = machine.succeed(f"systemctl show -P MainPID {unit}").strip()
        invocation = machine.succeed(f"systemctl show -P InvocationID {unit}").strip()
        machine.succeed("/run/current-system/specialisation/repository/bin/switch-to-configuration test")
        # ExecReload only signals: the reload completes in the server, shortly after.
        machine.wait_until_succeeds(f"test \"$(sqlite3 {db} \"SELECT count(*) FROM repo WHERE name = 'demo' AND archived_at IS NULL\")\" = 1", timeout=30)
        public_id = sql("SELECT public_id FROM repo WHERE name = 'demo'")
        machine.succeed(f"test -d /var/lib/kajiba-git/repos/{public_id}.git")
        assert machine.succeed(f"systemctl show -P MainPID {unit}").strip() == pid
        assert machine.succeed(f"systemctl show -P InvocationID {unit}").strip() == invocation
        events = invocation_events()
        reloaded = [e for e in events if e["event"] == "config-reloaded"]
        assert [e["generation"] for e in reloaded] == [2], f"reloads: {events}"
        assert not [e for e in events if e["event"] in ("config-reload-failed", "config-reload-partial")], f"{events}"
        assert_ready()

    with subtest("a restart completes within a few seconds"):
        started = machine.succeed("date +%s.%N")
        machine.succeed(f"systemctl restart {unit}")
        elapsed = float(machine.succeed("date +%s.%N")) - float(started)
        assert elapsed < 10, f"restart took {elapsed:.1f} s"
        assert_ready()

    with subtest("readyz never refuses a connection across server restarts"):
        # Earlier lifecycle starts also consume the production five-start budget.
        machine.succeed(f"systemctl reset-failed {unit}")
        machine.succeed("rm -f /tmp/probe-stop /tmp/probe-codes; (while test ! -e /tmp/probe-stop; do curl -s --max-time 15 -o /dev/null http://127.0.0.1:8080/readyz; echo $? >> /tmp/probe-codes; sleep 0.01; done; touch /tmp/probe-finished) >/tmp/probe-output 2>&1 &")
        machine.wait_until_succeeds("test $(grep -c '^0$' /tmp/probe-codes) -ge 3")
        for _ in range(3):
            machine.succeed(f"systemctl restart {unit}")
        machine.succeed("touch /tmp/probe-stop")
        machine.wait_until_succeeds("test -e /tmp/probe-finished")
        codes = machine.succeed("cat /tmp/probe-codes").splitlines()
        assert "7" not in codes, f"connection refused: {codes}"
        assert codes.count("0") >= 3, f"no successful probes: {codes}"
        assert_ready()

    with subtest("git restart drains an in-flight signed push and retains its sockets"):
        machine.succeed(
            "install -m 600 ${./fixtures/ssh-admin-key} /root/push-key",
            "ssh-keyscan -p 2222 127.0.0.1 > /root/known_hosts",
            "git init -b main /root/push",
            "git -C /root/push config user.name Alice",
            "git -C /root/push config user.email alice@example.org",
            "git -C /root/push config gpg.format ssh",
            "git -C /root/push config user.signingkey /root/push-key",
            "git -C /root/push commit --allow-empty -S -m initial",
            "touch /var/lib/kajiba-git/hold-push",
        )
        ssh = "ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/known_hosts -i /root/push-key -p 2222"
        push = f"env GIT_SSH_COMMAND={shlex.quote(ssh)} git -C /root/push push kajiba-git@127.0.0.1:alice/demo.git HEAD:refs/heads/main"
        machine.succeed(f"(status=0; {push} >/tmp/push-output 2>&1 || status=$?; echo $status >/tmp/push-status) </dev/null >/dev/null 2>&1 &")
        machine.wait_until_succeeds("test -e /var/lib/kajiba-git/push-started")
        invocation = machine.succeed("systemctl show -P InvocationID kajiba-git.service").strip()
        sockets = machine.succeed("stat -c '%i' /run/kajiba-git/local.sock /run/kajiba-git/ssh.sock")
        machine.succeed("systemctl restart --no-block kajiba-git.service")
        machine.wait_until_succeeds(f"journalctl _SYSTEMD_INVOCATION_ID={invocation} -o cat | grep -F stopping")
        # A new signed push must queue until the old generation has finished draining.
        queued_push = push.replace("HEAD:refs/heads/main", "HEAD:refs/heads/queued")
        machine.succeed(f"(status=0; {queued_push} >/tmp/queued-push-output 2>&1 || status=$?; echo $status >/tmp/queued-push-status) </dev/null >/dev/null 2>&1 &")
        machine.wait_until_succeeds("ss -xln | awk '$5 == \"/run/kajiba-git/ssh.sock\" && $3 > 0 { found=1 } END { exit !found }'", timeout=10)
        machine.succeed("test ! -e /tmp/queued-push-status")
        machine.succeed("test ! -e /tmp/push-status", "rm /var/lib/kajiba-git/hold-push")
        machine.wait_until_succeeds("test -e /tmp/push-status", timeout=45)
        assert machine.succeed("cat /tmp/push-status").strip() == "0", machine.succeed("cat /tmp/push-output")
        machine.wait_until_succeeds("test -e /tmp/queued-push-status", timeout=45)
        assert machine.succeed("cat /tmp/queued-push-status").strip() == "0", machine.succeed("cat /tmp/queued-push-output")
        machine.wait_for_unit("kajiba-git.service")
        assert machine.succeed("systemctl show -P InvocationID kajiba-git.service").strip() != invocation
        assert machine.succeed("stat -c '%i' /run/kajiba-git/local.sock /run/kajiba-git/ssh.sock") == sockets
        assert "drain-timeout" not in machine.succeed(f"journalctl _SYSTEMD_INVOCATION_ID={invocation} -o cat")
        expected = machine.succeed("git -C /root/push rev-parse HEAD").strip()
        directory = sql("SELECT public_id FROM repo WHERE name = 'demo'")
        actual = machine.succeed(f"runuser -u kajiba-git -- git --git-dir=/var/lib/kajiba-git/repos/{directory}.git rev-parse refs/heads/main").strip()
        assert actual == expected
        assert machine.succeed(f"runuser -u kajiba-git -- git --git-dir=/var/lib/kajiba-git/repos/{directory}.git rev-parse refs/heads/queued").strip() == expected
        assert_ready()

    with subtest("every service log line validates against the nisshi schema"):
        validate_logs(machine, unit)
        validate_logs(machine, "kajiba-git.service")
  '';
}
