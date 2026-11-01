# J8 task 7: scheduled online dumps, rotation and failed-copy recovery.
args:
import ./ci-common.nix args {
  name = "ops-timers";
  forgeConfig = { lib, ... }: {
    services.kajiba-server.dump.keep = 2;
    # Fire the real timer once shortly after boot; subsequent invocations below are manual.
    systemd.timers.kajiba-dump.timerConfig = {
      OnCalendar = lib.mkForce "";
      OnActiveSec = "5s";
    };
  };
  script = ''
    def dumps():
        return forge.succeed("find /var/lib/kajiba-dumps -mindepth 1 -maxdepth 1 -type d -name 'dump-*' | sort").splitlines()

    def invoke_dump():
        forge.succeed("systemctl start kajiba-dump.service")
        assert forge.succeed("systemctl show -P Result kajiba-dump.service").strip() == "success"

    with subtest("timer invokes the admitted dump command as the server identity"):
        forge.wait_for_unit("kajiba-dump.timer")
        forge.wait_until_succeeds("test -n \"$(find /var/lib/kajiba-dumps -maxdepth 1 -name 'dump-*' -type d -print -quit)\"")
        forge.wait_until_succeeds("test \"$(systemctl show -P ActiveState kajiba-dump.service)\" = inactive")
        forge.succeed("systemctl stop kajiba-dump.timer")
        assert len(dumps()) == 1
        first = dumps()[0]
        assert forge.succeed(f"stat -c '%U:%G:%a' {first}").strip() == "kajiba-server:kajiba-server:700"
        assert forge.succeed("systemctl show -P User kajiba-dump.service").strip() == "kajiba-server"
        assert any(line.get("event") == "dump-done" for line in unit_lines(forge, "kajiba-server.service"))

    with subtest("successful daily command rotates only completed dumps"):
        forge.succeed("mkdir /var/lib/kajiba-dumps/.partial-interrupted /var/lib/kajiba-dumps/unrelated")
        invoke_dump()
        assert len(dumps()) == 2 and first in dumps()
        invoke_dump()
        assert len(dumps()) == 2 and first not in dumps()
        forge.succeed("test -d /var/lib/kajiba-dumps/.partial-interrupted", "test -d /var/lib/kajiba-dumps/unrelated")
        good = dumps()
        hashes = {path: forge.succeed(f"sha256sum {path}/manifest.json").split()[0] for path in good}
        metric = forge.succeed(f"curl -fsS {metrics_url} | grep '^kajiba_dump_last_success_timestamp_seconds '").strip()

    with subtest("a failed scheduled dump preserves the last good backups and metric"):
        forge.succeed("mkdir -p /var/lib/kajiba-git/tmp/orphans",
                      "ln -s missing /var/lib/kajiba-git/tmp/orphans/unsafe-dump")
        try:
            forge.fail("systemctl start kajiba-dump.service")
            assert dumps() == good
            for path in good:
                assert forge.succeed(f"sha256sum {path}/manifest.json").split()[0] == hashes[path]
            assert forge.succeed(f"curl -fsS {metrics_url} | grep '^kajiba_dump_last_success_timestamp_seconds '").strip() == metric
            assert forge.succeed("find /var/lib/kajiba-dumps -maxdepth 1 -name '.partial-*'").splitlines() == ["/var/lib/kajiba-dumps/.partial-interrupted"]
        finally:
            forge.succeed("rm /var/lib/kajiba-git/tmp/orphans/unsafe-dump")
        forge.succeed("systemctl reset-failed kajiba-dump.service")
        invoke_dump()
        assert len(dumps()) == 2 and dumps() != good
        assert any(line.get("event") == "dump-failed" for line in unit_lines(forge, "kajiba-dump.service"))
        validate_logs(forge, "kajiba-dump.service")
  '';
}
