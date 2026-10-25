# J1 task 28: the server becomes ready through the module, reconciles a declared user and key,
# keeps its rows across a reboot, re-migrates a wiped stateDir, and refuses a second instance
# on the same stateDir (0012, 0014, 0015, 0023).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
let
  inherit (pkgs) lib;
  stateDir = "/var/lib/kajiba-server";
  keyBlob = "AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea";
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
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      inherit stateDir;
      http.publicUrl = "https://forge.example.org";
      users.alice = {
        isAdmin = true;
        sshKeys = [ "ssh-ed25519 ${keyBlob} alice@example.org" ];
      };
    };
    environment.systemPackages = [
      pkgs.curl
      pkgs.sqlite
    ];
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
            " FROM user u JOIN ssh_key k ON k.user_id = u.id"
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

    with subtest("every service log line validates against the nisshi schema"):
        validate_logs(machine, unit)
        validate_logs(machine, "kajiba-git.service")
  '';
}
