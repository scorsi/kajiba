# J8 tasks 7 and 9: scheduled dumps/rotation and admitted weekly gc.
{ pkgs, ... }@args:
let
  inherit (pkgs) lib;
  heldCommands = pkgs.writeShellScriptBin "git" ''
    for arg in "$@"; do
      case "$arg" in
        gc) id -un >> /var/lib/kajiba-fixtures/gc-entered ;;
        fsck)
          if [ -e /var/lib/kajiba-fixtures/hold-copy ]; then
            touch /var/lib/kajiba-fixtures/copy-entered
            while [ -e /var/lib/kajiba-fixtures/hold-copy ]; do sleep 0.02; done
          fi
          ;;
      esac
    done
    exec ${pkgs.git}/bin/git "$@"
  '';
in
import ./ci-common.nix args {
  name = "ops-timers";
  forgeConfig = {
    services.kajiba-server.dump.keep = 2;
    # Fire the real timer once shortly after boot; subsequent invocations below are manual.
    systemd.timers.kajiba-dump.timerConfig = {
      OnCalendar = lib.mkForce "";
      OnActiveSec = "5s";
    };
    # Invoke gc explicitly so the calendar cannot race the dump rotation assertions.
    systemd.timers.kajiba-gc.wantedBy = lib.mkForce [ ];
    systemd.tmpfiles.rules = [ "d /var/lib/kajiba-fixtures 0777 root root -" ];
    systemd.services.kajiba-git.path = lib.mkBefore [ heldCommands ];
    systemd.services.kajiba-server.path = lib.mkBefore [ heldCommands ];
    systemd.services.kajiba-server.serviceConfig.ReadWritePaths = [ "/var/lib/kajiba-fixtures" ];
    systemd.services.kajiba-git.serviceConfig.ReadWritePaths = [ "/var/lib/kajiba-fixtures" ];
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

    with subtest("weekly gc service uses the daemon identity and admission gate during a dump"):
        forge.succeed("systemctl start kajiba-gc.timer")
        forge.wait_for_unit("kajiba-gc.timer")
        forge.succeed("systemctl stop kajiba-gc.timer", "systemctl start kajiba-gc.service")
        assert forge.succeed("systemctl show -P User kajiba-gc.service").strip() == "kajiba-git"
        assert forge.succeed("systemctl show -P Result kajiba-gc.service").strip() == "success"
        children = forge.succeed("cat /var/lib/kajiba-fixtures/gc-entered").splitlines()
        assert children and set(children) == {"kajiba-git"}, children
        exits = [e for e in unit_lines(forge, "kajiba-git.service") if e.get("event") == "session-exited" and e.get("kind") == "maintenance"]
        assert any(e["status"] == 0 for e in exits), exits
        forge.succeed("touch /var/lib/kajiba-fixtures/hold-copy")
        forge.succeed("(status=0; systemctl start kajiba-dump.service || status=$?; echo $status >/tmp/held-dump.status) </dev/null >/tmp/held-dump.out 2>&1 &")
        try:
            forge.wait_until_succeeds("test -e /var/lib/kajiba-fixtures/copy-entered")
            forge.succeed("test ! -e /tmp/held-dump.status")
            forge.fail("systemctl start kajiba-gc.service")
            assert forge.succeed("systemctl show -P Result kajiba-gc.service").strip() == "exit-code"
            assert any("maintenance" in e.get("err", "").lower() for e in unit_lines(forge, "kajiba-gc.service"))
            assert forge.succeed("cat /var/lib/kajiba-fixtures/gc-entered").splitlines() == children
        finally:
            forge.succeed("rm /var/lib/kajiba-fixtures/hold-copy")
        forge.wait_until_succeeds("test -e /tmp/held-dump.status")
        assert forge.succeed("cat /tmp/held-dump.status").strip() == "0", forge.succeed("cat /tmp/held-dump.out")
        forge.succeed("systemctl reset-failed kajiba-gc.service", "systemctl start kajiba-gc.service")
        assert forge.succeed("systemctl show -P Result kajiba-gc.service").strip() == "success"
        retried = forge.succeed("cat /var/lib/kajiba-fixtures/gc-entered").splitlines()
        assert len(retried) > len(children) and set(retried) == {"kajiba-git"}, retried
        validate_logs(forge, "kajiba-gc.service")
  '';
}
