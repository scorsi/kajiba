# J3 task 23: the forced-command boundary, without the sshd/module work of tasks 24–26.
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
let
  inherit (pkgs) lib;
  control = "/var/lib/kajiba-git/test";
  # Test-only wrappers allow a real receive-pack to pause after updating refs.
  hooks = pkgs.runCommand "kajiba-transport-test-hooks" { } ''
    mkdir "$out"
    cat > "$out/pre-receive" <<'HOOK'
    #!${pkgs.runtimeShell}
    exec ${lib.getExe kajibaGitPackage} hook pre-receive
    HOOK
    cat > "$out/post-receive" <<'HOOK'
    #!${pkgs.runtimeShell}
    ${lib.getExe kajibaGitPackage} hook post-receive || exit $?
    if test -e ${control}/hold; then
      ${pkgs.coreutils}/bin/touch ${control}/entered
      while ! test -e ${control}/release; do ${pkgs.coreutils}/bin/sleep 0.05; done
    fi
    HOOK
    chmod +x "$out/"*
  '';
  gitConfig = (pkgs.formats.toml { }).generate "kajiba-transport-test.toml" {
    hooks_path = toString hooks;
  };
  ssh = pkgs.writeShellScript "kajiba-test-ssh" ''
    export SSH_ORIGINAL_COMMAND="$2"
    exec ${pkgs.util-linux}/bin/runuser -u kajiba-git -- ${pkgs.runtimeShell} -c '
      echo $$ > ${control}/helper
      exec ${lib.getExe kajibaGitPackage} serve-ssh --user "$KAJIBA_TEST_USER"
    '
  '';
in
pkgs.testers.runNixOSTest {
  name = "kajiba-ssh-transport";
  nodes.forge = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      http.publicUrl = "https://forge.example.org";
      users = {
        alice.signingKeys = [ (builtins.readFile ./fixtures/ssh-signing-key.pub) ];
        reader = { };
      };
      repos.demo = {
        owner = "alice";
        description = "SSH transport fixture";
        previousNames = [ "alice/old" ];
      };
    };
    systemd.services.kajiba-git.serviceConfig.ExecStart =
      lib.mkForce "${lib.getExe kajibaGitPackage} serve --config ${gitConfig}";
    environment.systemPackages = [
      pkgs.git
      pkgs.openssh
      pkgs.sqlite
      pkgs.procps
      pkgs.curl
    ];
  };
  testScript = testLib + ''
    import json

    unit = "kajiba-server.service"
    db = "/var/lib/kajiba-server/db/kajiba.sqlite"
    control = "${control}"

    def sql(query):
        return forge.succeed(f"sqlite3 {db} {shlex.quote(query)}").strip()

    def git(args, user=None, success=True):
        env = f"GIT_SSH_COMMAND=${ssh} GIT_SSH_VARIANT=simple KAJIBA_TEST_USER={user or alice}"
        command = f"env {env} git {args}"
        if success:
            return forge.succeed(command)
        code, out = forge.execute(command + " 2>&1")
        assert code != 0, out
        return out

    def direct(command, user=None):
        return forge.execute(f"runuser -u kajiba-git -- env SSH_ORIGINAL_COMMAND={shlex.quote(command)} kajiba-git serve-ssh --user {user or alice} </dev/null 2>&1")

    def started():
        forge.succeed("journalctl --sync")
        lines = forge.succeed("journalctl _SYSTEMD_UNIT=kajiba-git.service -o cat --no-pager")
        return sum(json.loads(line).get("event") == "session-started" for line in lines.splitlines())

    def wait_pending_empty():
        forge.wait_until_succeeds(f"test $(sqlite3 {db} 'SELECT COUNT(*) FROM pending_push') = 0")

    forge.wait_for_unit(unit)
    alice = sql("SELECT public_id FROM user WHERE name='alice'")
    reader = sql("SELECT public_id FROM user WHERE name='reader'")
    forge.succeed(f"install -d -o kajiba-git -g kajiba-git {control}")
    # This key is a committed test fixture, never a deployment credential.
    forge.succeed("install -m 600 ${./fixtures/ssh-signing-key} /root/signing-key")
    forge.succeed("git init -b main /root/work", "git -C /root/work config user.name Alice", "git -C /root/work config user.email alice@example.org",
                  "git -C /root/work config gpg.format ssh", "git -C /root/work config user.signingkey /root/signing-key",
                  "git -C /root/work commit --allow-empty -S -m initial")

    with subtest("signed push, clone, fetch, redirects and optional .git"):
        git("-C /root/work push host:alice/demo main")
        wait_pending_empty()
        assert sql("SELECT COUNT(*) FROM event WHERE kind='push'") == "1"
        git("clone host:alice/old.git /root/clone")
        git("-C /root/work commit --allow-empty -S -m next")
        git("-C /root/work push host:alice/demo.git main")
        git("-C /root/clone fetch host:/alice/demo main", user=reader)
        assert forge.succeed("git -C /root/clone rev-parse FETCH_HEAD").strip() == forge.succeed("git -C /root/work rev-parse HEAD").strip()
        wait_pending_empty()

    with subtest("serve-ssh returns the child's nonzero exit status"):
        code, out = direct("git-upload-pack 'alice/demo'")
        assert code == 128, (code, out)  # upload-pack sees EOF before a client request.

    with subtest("unknown repository, invalid commands and read-only push fail before a child starts"):
        before = started()
        # Root bypasses the socket's mode but must still fail its peer-uid check.
        forge.fail(f"env SSH_ORIGINAL_COMMAND=\"git-upload-pack 'alice/demo'\" kajiba-git serve-ssh --user {alice} </dev/null")
        assert "repository not found" in git("ls-remote host:alice/missing", success=False)
        assert "repository role required" in git("-C /root/work push host:alice/demo main", user=reader, success=False)
        for command in ["sh -c 'id'", "git-upload-pack '../demo'", "git-upload-pack 'alice/demo' extra"]:
            code, out = direct(command)
            assert code != 0 and "invalid SSH command" in out, out
        assert started() == before

    with subtest("archived clone, fetch and push including redirects are refused explicitly"):
        sql("UPDATE repo SET archived_at=1 WHERE config_key='demo'")
        before = started()
        for args in ["clone host:alice/demo /root/archived", "-C /root/clone fetch host:alice/old.git", "-C /root/work push host:alice/demo.git main"]:
            assert "archived" in git(args, success=False)
        assert started() == before
        sql("UPDATE repo SET archived_at=NULL WHERE config_key='demo'")

    with subtest("server down refuses sessions before any child starts"):
        forge.succeed(f"systemctl stop {unit}")
        before = started()
        for service in ["upload-pack", "receive-pack"]:
            code, out = direct(f"git-{service} 'alice/demo'")
            assert code != 0 and "server unavailable" in out, out
        assert started() == before
        assert sql("SELECT COUNT(*) FROM pending_push") == "0"
        forge.succeed(f"systemctl start {unit}")

    def held_push():
        forge.succeed(f"rm -f {control}/entered {control}/release; touch {control}/hold")
        git("-C /root/work commit --allow-empty -S -m held")
        command = f"env GIT_SSH_COMMAND=${ssh} GIT_SSH_VARIANT=simple KAJIBA_TEST_USER={alice} git -C /root/work push host:alice/demo main"
        forge.succeed(f"({command}; echo $? >/root/push.status) >/root/push.out 2>&1 </dev/null &")
        forge.wait_for_file(f"{control}/entered")
        assert sql("SELECT COUNT(*) FROM pending_push") == "1"
        return sql("SELECT COUNT(*) FROM event WHERE kind='push'")

    def release_push(events):
        forge.succeed(f"touch {control}/release")
        forge.wait_for_file("/root/push.status")
        wait_pending_empty()
        assert int(sql("SELECT COUNT(*) FROM event WHERE kind='push'")) == int(events) + 1
        forge.succeed(f"rm -f {control}/hold /root/push.status")

    with subtest("killing serve-ssh leaves receive-pack under the daemon until normal exit"):
        events = held_push()
        forge.succeed(f"kill -KILL $(cat {control}/helper)")
        child = forge.succeed("pgrep -u kajiba-git -f 'git .*receive-pack'").strip()
        parent = forge.succeed(f"ps -o ppid= -p {child}").strip()
        assert parent == forge.succeed("systemctl show -P MainPID kajiba-git.service").strip()
        assert sql("SELECT COUNT(*) FROM event WHERE kind='push'") == events
        release_push(events)
        forge.wait_until_succeeds("journalctl _SYSTEMD_UNIT=kajiba-git.service -o cat | grep '\"event\":\"session-exited\"' | tail -1 | grep '\"status\":0'")

    with subtest("surviving receive-pack blocks reconciliation across server restart until SessionExited"):
        events = held_push()
        server_pid = forge.succeed(f"systemctl show -P MainPID {unit}").strip()
        daemon_pid = forge.succeed("systemctl show -P MainPID kajiba-git.service")
        forge.succeed(f"systemctl kill --signal=KILL --kill-whom=main {unit}")
        forge.wait_until_succeeds(f"test $(systemctl show -P MainPID {unit}) != {server_pid} && systemctl is-active {unit}")
        forge.wait_until_succeeds("curl -fsS http://127.0.0.1:8080/readyz")
        assert forge.succeed("systemctl show -P MainPID kajiba-git.service") == daemon_pid
        assert sql("SELECT COUNT(*) FROM pending_push") == "1"
        assert sql("SELECT COUNT(*) FROM event WHERE kind='push'") == events
        assert "unfinished session" in git("-C /root/work push host:alice/demo main", success=False)
        release_push(events)
        assert forge.succeed("cat /root/push.out")
        git("-C /root/work push host:alice/demo main")

    with subtest("push and session logs retain admission correlation without duplicate replay"):
        forge.succeed("journalctl --sync")
        def journal(service):
            return [json.loads(line) for line in forge.succeed(f"journalctl _SYSTEMD_UNIT={service} -o cat --no-pager").splitlines()]

        server_logs = journal(unit)
        daemon_logs = journal("kajiba-git.service")
        pushes = [line for line in server_logs if line.get("event") == "push"]
        events = json.loads(forge.succeed(f"sqlite3 -json {db} \"SELECT correlation_id, payload FROM event WHERE kind='push' ORDER BY id\""))
        assert len(pushes) == len(events), (pushes, events)
        for push, event in zip(pushes, events):
            assert push["request_id"] == event["correlation_id"]
            assert push["repo"] == "alice/demo" and push["user"] == "alice"
            for field, value in json.loads(event["payload"]).items():
                assert push[field] == value, (push, event)
        refusals = [line for line in server_logs if line.get("event") == "push-refused"]
        assert any(line["reason"] == "repository role required" for line in refusals)
        for line in refusals:
            assert line["level"] == "warn" and line["request_id"] and line["repo"]
            assert "ref" in line and line["reason"]
        starts = [line for line in daemon_logs if line.get("event") == "session-started"]
        exits = [line for line in daemon_logs if line.get("event") == "session-exited"]
        assert len(starts) == len(exits) == len({line["session"] for line in exits})
        for start in starts:
            exit = next(line for line in exits if line["session"] == start["session"])
            assert start["level"] == exit["level"] == "info"
            assert start["kind"] in ["receive_pack", "upload_pack"]
            for field in ["session", "kind", "repo", "request_id"]:
                assert start[field] and start[field] == exit[field], (start, exit)
            assert isinstance(exit["status"], int)
        for push in pushes:
            assert any(start["kind"] == "receive_pack" and start["request_id"] == push["request_id"] for start in starts)
        assert "ssh-ed25519" not in json.dumps(server_logs + daemon_logs)

    validate_logs(forge, unit)
    validate_logs(forge, "kajiba-git.service")
  '';
}
