# J1 task 28: the server becomes ready through the module, reconciles a declared user and key,
# keeps its rows across a reboot, re-migrates a wiped stateDir, and refuses a second instance
# on the same stateDir (0012, 0014, 0015, 0023).
{
  pkgs,
  kajibaServerPackage,
  testLib,
}:
let
  inherit (pkgs) lib;
  stateDir = "/var/lib/kajiba";
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
        # Root reads while the service runs, so the WAL files already exist and stay kajiba's.
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
        assert "status=0/0" in machine.succeed(f"systemctl show -p ExecStartPost {unit}")
        for path in ["healthz", "readyz"]:
            code = machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' http://127.0.0.1:8080/{path}")
            assert code == "200", f"/{path}: {code}"


    with subtest("ready through the module after migrating a fresh stateDir"):
        assert_ready()
        events = invocation_events()
        assert applied(events) == migrations, events
        assert "ready" in {event["event"] for event in events}, events

    with subtest("declared user and SSH key reconciled into the database"):
        first = identity()
        assert without_ids(first) == ["alice", "1", "config", "1", "auth", "ssh-ed25519", "1", "config"], first

    with subtest("a second serve on the same stateDir is refused by kajiba.lock"):
        machine.fail("runuser -u kajiba -- kajiba-server serve --config /etc/kajiba/server.toml 2>/tmp/second.log")
        lines = [json.loads(line) for line in machine.succeed("cat /tmp/second.log").splitlines()]
        fatal = [line for line in lines if line["event"] == "fatal"]
        assert len(fatal) == 1 and "another instance holds ${stateDir}/kajiba.lock" in fatal[0]["err"], lines
        validate_log_output(machine, "cat /tmp/second.log")
        assert_ready()

    with subtest("rows survive a reboot with the persisted stateDir, without re-migration"):
        machine.shutdown()
        machine.start()
        assert_ready()
        assert applied(invocation_events()) == set()
        assert identity() == first

    with subtest("a wiped stateDir is migrated again from scratch"):
        machine.succeed(f"systemctl stop {unit}", "rm -rf ${stateDir}", f"systemctl start {unit}")
        assert_ready()
        assert applied(invocation_events()) == migrations
        fresh = identity()
        # Same declared content, new rows: the reconciliation re-created them from the config.
        assert without_ids(fresh) == without_ids(first), fresh
        assert fresh[0] != first[0] and fresh[5] != first[5], fresh

    with subtest("every service log line validates against the nisshi schema"):
        validate_logs(machine, unit)
  '';
}
