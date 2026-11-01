# J8 task 8: daemon-owned foreground gc and the shared repository/dump gates.
{ pkgs, kajibaGitPackage, ... }@args:
let
  inherit (pkgs) lib;
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
        gc) latch=gc ;;
        fsck) latch=copy ;;
      esac
    done
    if [ "''${latch:-}" = gc ]; then
      test "$(id -un)" = kajiba-git || exit 24
      if [ -e /var/lib/kajiba-fixtures/fail-gc ]; then exit 23; fi
    fi
    if [ -n "''${latch:-}" ] && [ -e /var/lib/kajiba-fixtures/hold-$latch ]; then
      touch /var/lib/kajiba-fixtures/$latch-entered
      while [ -e /var/lib/kajiba-fixtures/hold-$latch ]; do sleep 0.02; done
    fi
    exec ${pkgs.git}/bin/git "$@"
  '';
in
import ./ci-common.nix (args // { kajibaGitPackage = heldGit; }) {
  name = "ops-gc";
  forgeConfig = {
    systemd.tmpfiles.rules = [ "d /var/lib/kajiba-fixtures 0777 root root -" ];
    systemd.services.kajiba-git.path = lib.mkBefore [ heldCommands ];
    systemd.services.kajiba-server.path = lib.mkBefore [ heldCommands ];
    systemd.services.kajiba-server.serviceConfig.ReadWritePaths = [ "/var/lib/kajiba-fixtures" ];
    systemd.services.kajiba-git.serviceConfig.ReadWritePaths = [ "/var/lib/kajiba-fixtures" ];
  };
  script = ''
    gc = "runuser -u kajiba-git -- kajiba-git gc"
    remote = "kajiba-git@127.0.0.1:alice/demo.git"

    def background(command, name):
        forge.succeed(f"(status=0; {command} >/tmp/{name}.out 2>/tmp/{name}.err || status=$?; echo $status >/tmp/{name}.status) </dev/null >/dev/null 2>&1 &")

    def done(name, expected=0):
        forge.wait_until_succeeds(f"test -e /tmp/{name}.status")
        assert int(forge.succeed(f"cat /tmp/{name}.status")) == expected, forge.succeed(f"cat /tmp/{name}.err")

    def push_command(sha):
        return f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git -C /root/work push {remote} {sha}:refs/heads/main"

    def forge_refs():
        return forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git ls-remote --symref {remote}")

    with subtest("an admitted signed push excludes gc until its hook and child finish"):
        initial = commit("", "before gc")
        assert finished(push(initial)) == "succeeded"
        sha = commit("", "held signed push")
        forge.succeed("chmod 777 /var/lib/kajiba-fixtures", "touch /var/lib/kajiba-git/hold-hook")
        background(push_command(sha), "push")
        forge.wait_until_succeeds("test -e /var/lib/kajiba-git/hook-entered")
        code, out = forge.execute(gc + " 2>&1")
        assert code != 0 and "unfinished session" in out.lower(), out
        forge.succeed("test ! -e /var/lib/kajiba-fixtures/gc-entered", "rm /var/lib/kajiba-git/hold-hook")
        done("push")
        forge.wait_until_succeeds("test $(sqlite3 /var/lib/kajiba-server/db/kajiba.sqlite 'SELECT count(*) FROM pending_push') = 0")
        before = forge_refs()
        forge.succeed(gc)
        assert forge_refs() == before
        forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git clone {remote} /root/after-gc")
        assert forge.succeed("git -C /root/after-gc rev-parse HEAD").strip() == sha
        assert forge.succeed("cat /root/after-gc/flake.nix") == forge.succeed("cat /root/work/flake.nix")

    with subtest("foreground gc excludes a concurrent signed push and releases on exit"):
        sha = commit("", "push after gc")
        forge.succeed("touch /var/lib/kajiba-fixtures/hold-gc")
        background(gc, "gc")
        forge.wait_until_succeeds("test -e /var/lib/kajiba-fixtures/gc-entered")
        code, out = forge.execute(push_command(sha) + " 2>&1")
        assert code != 0 and "unfinished session" in out.lower(), out
        assert forge_refs() == before
        forge.succeed("test ! -e /tmp/gc.status", "rm /var/lib/kajiba-fixtures/hold-gc")
        done("gc")
        assert finished(push(sha)) == "succeeded"

    with subtest("gc reports child failure and the next admitted operation succeeds"):
        before = forge_refs()
        forge.succeed("touch /var/lib/kajiba-fixtures/fail-gc")
        code, out = forge.execute(gc + " 2>&1")
        assert code == 23, out
        forge.succeed("rm /var/lib/kajiba-fixtures/fail-gc", gc)
        assert forge_refs() == before
        exits = [e for e in unit_lines(forge, "kajiba-git.service") if e.get("event") == "session-exited" and e.get("kind") == "maintenance"]
        assert any(e["status"] == 23 for e in exits), exits
        assert any(e["status"] == 0 for e in exits), exits

    with subtest("a held dump refuses gc with maintenance then permits retry"):
        forge.succeed("touch /var/lib/kajiba-fixtures/hold-copy")
        background("kajiba-server admin dump", "dump")
        forge.wait_until_succeeds("test -e /var/lib/kajiba-fixtures/copy-entered")
        code, out = forge.execute(gc + " 2>&1")
        assert code != 0 and "maintenance" in out.lower(), out
        assert forge_refs() == before
        forge.succeed("rm /var/lib/kajiba-fixtures/hold-copy")
        done("dump")
        forge.succeed(gc)
        assert forge_refs() == before
  '';
}
