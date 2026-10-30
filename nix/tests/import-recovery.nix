# Quality review items 10/11: interrupt a real Fetch, not a completed fixture fetch.
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
let
  inherit (pkgs) lib;
  control = "/var/lib/kajiba-fixtures/import-control";
  # Exercise the actual deadline path without waiting an hour; production is unchanged.
  shortDeadlineGit = kajibaGitPackage.overrideAttrs (old: {
    postPatch = (old.postPatch or "") + ''
      substituteInPlace crates/kajiba-gitstore/src/lib.rs \
        --replace-fail 'pub const IMPORT_TIMEOUT: Duration = Duration::from_secs(60 * 60);' \
                       'pub const IMPORT_TIMEOUT: Duration = Duration::from_secs(30);'
    '';
  });
  deadlineUnit = pkgs.writeText "kajiba-import-deadline.conf" ''
    [Service]
    ExecStart=
    ExecStart=${lib.getExe shortDeadlineGit} serve --config /etc/kajiba/git.toml
  '';
in
pkgs.testers.runNixOSTest {
  name = "kajiba-import-recovery";

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
      users.alice = {
        sshKeys = [ (builtins.readFile ./fixtures/ssh-writer-key.pub) ];
        signingKeys = [ (builtins.readFile ./fixtures/ssh-signing-key.pub) ];
      };
      repos = lib.genAttrs [ "daemon" "server" "both" "timeout" ] (_: {
        owner = "alice";
        description = "Controlled import recovery";
      });
    };
    # Observe the failed unit and its pending row before explicitly restarting it.
    systemd.services.kajiba-server = {
      unitConfig.StartLimitIntervalSec = 0;
      serviceConfig.Restart = lib.mkForce "no";
    };
    systemd.services.kajiba-git = {
      unitConfig.StartLimitIntervalSec = 0;
      serviceConfig.Restart = lib.mkForce "no";
    };
    systemd.services.import-source = {
      path = [ pkgs.git ];
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python ${./fixtures/import-source.py} /var/lib/kajiba-fixtures/source.git ${control}";
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
    metrics = "http://127.0.0.1:9433/metrics"
    bundle = "/var/lib/kajiba-fixtures/source.bundle"

    def rows(query):
        out = forge.succeed(f"sqlite3 -cmd '.timeout 5000' -json {db} {shlex.quote(query)}")
        return json.loads(out or "[]")

    def pending():
        return rows("SELECT * FROM pending_push ORDER BY id")

    def events():
        return rows("SELECT * FROM event WHERE kind='push' ORDER BY id")

    def journal(service):
        forge.succeed("journalctl --sync")
        return [json.loads(line) for line in forge.succeed(f"journalctl _SYSTEMD_UNIT={service} -o cat --no-pager").splitlines()]

    def generation():
        return [line["generation"] for line in journal(unit) if line.get("event") == "git-side-connected"][-1]

    def ready():
        forge.wait_until_succeeds("curl -fsS http://127.0.0.1:8080/readyz")

    def idle():
        sample = 'kajiba_git_sessions_active{kind="import"} 0'
        forge.wait_until_succeeds(f"curl -fsS {metrics} | grep -Fx {shlex.quote(sample)}")

    def settled():
        forge.wait_until_succeeds(f"test $(sqlite3 {db} 'SELECT COUNT(*) FROM pending_push') = 0")
        idle()

    def crash(service):
        forge.succeed(f"systemctl kill --signal=KILL --kill-whom=main {service}")
        forge.wait_until_succeeds(f"systemctl is-failed {service}")
        forge.wait_until_succeeds(f"test $(systemctl show -P MainPID {service}) = 0")

    def finished(key, success=False):
        forge.wait_until_succeeds(f"test -s /root/{key}.status", timeout=45)
        status = int(forge.succeed(f"cat /root/{key}.status"))
        assert status != 124 and (status == 0) == success, forge.succeed(f"cat /root/{key}.out")

    def recovered(before, row, applied):
        settled()
        after = events()
        assert after[:len(before)] == before
        added = after[len(before):]
        assert [json.loads(event["payload"]) for event in added] == applied, added
        for event in added:
            assert event["repo_id"] == row["repo_id"]
            assert event["principal_kind"] == "system" and event["principal_id"] is None
            assert event["correlation_id"] == row["correlation_id"]
        # Repeat both startup recovery and the daemon's session snapshot.
        forge.succeed(f"systemctl restart {unit}")
        ready()
        settled()
        assert events() == after

    forge.wait_for_unit(unit)
    ready()
    expected = make_demo(forge, bundle)
    forge.succeed(
        f"git clone --bare {bundle} /var/lib/kajiba-fixtures/source.git",
        f"mkdir -p {control}",
        "install -m600 ${./fixtures/ssh-writer-key} /root/auth-key",
        "install -m600 ${./fixtures/ssh-signing-key} /root/signing-key",
        "systemctl start import-source.service",
    )
    forge.wait_for_open_port(9425)

    for key in ["daemon", "server", "both", "timeout"]:
        with subtest(f"mid-fetch {key}: child lifetime, durable recovery and next admission"):
            if key == "timeout":
                previous = generation()
                forge.succeed(
                    "install -Dm644 ${deadlineUnit} /run/systemd/system/kajiba-git.service.d/deadline.conf",
                    "systemctl daemon-reload",
                    f"systemctl restart {git_unit}",
                )
                # readyz stays OK across a fast restart; admission needs the new generation's snapshot.
                retry(lambda _: generation() != previous)
                ready()
            before = events()
            pid = forge.succeed(f"systemctl show -P MainPID {git_unit}").strip()
            source = f"http://127.0.0.1:9425/{key}"
            command = f"timeout 180 kajiba-server admin import alice/{key} {source}"
            forge.succeed(f"(status=0; {command} || status=$?; echo $status > /root/{key}.status) > /root/{key}.out 2>&1 < /dev/null &")
            # ListRemoteRefs cannot trip this barrier: a real fetch sent its want.
            forge.wait_for_file(f"{control}/{key}.entered")
            row, = pending()
            assert row["git_session_id"] and row["git_generation"] == generation()
            assert any(line.get("event") == "session-started" and line.get("session") == row["git_session_id"] and line.get("kind") == "import" for line in journal(git_unit))
            assert events() == before
            child = forge.succeed(f"pgrep -P {pid} -f 'git .*fetch'").strip()
            assert child.isdigit(), child
            directory = "/var/lib/kajiba-git/repos/" + rows(f"SELECT public_id FROM repo WHERE id={row['repo_id']}")[0]["public_id"] + ".git"
            assert refs(forge, directory, user="kajiba-git") == {}

            if key == "server":
                crash(unit)
                finished(key)
                forge.succeed(f"kill -0 {child}", f"systemctl start {unit}")
                ready()
                assert forge.succeed(f"systemctl show -P MainPID {git_unit}").strip() == pid
                assert generation() == row["git_generation"]
                forge.succeed(f"kill -0 {child}")
                assert pending() == [row] and events() == before
                forge.fail(f"kajiba-server admin import alice/{key} {bundle} > /root/blocked.out 2>&1")
                assert "in progress" in forge.succeed("cat /root/blocked.out")
                assert pending() == [row] and events() == before
                forge.succeed(f"touch {control}/{key}.release")
                recovered(before, row, json.loads(row["updates"]))
                forge.fail(f"kill -0 {child}")
                assert any(line.get("event") == "session-exited" and line.get("session") == row["git_session_id"] and line["status"] == 0 for line in journal(git_unit))
                assert refs(forge, directory, user="kajiba-git") == expected
                # The filled repository's gate must admit its next writer too.
                saved = events()
                forge.succeed(
                    "git -C /root/demo -c user.name=Alice -c user.email=alice@example.org -c gpg.format=ssh -c user.signingkey=/root/signing-key commit --allow-empty -S -m after-recovery",
                    "env GIT_SSH_COMMAND='ssh -i /root/auth-key -o BatchMode=yes -o IdentityAgent=none -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p 2222' git -C /root/demo push kajiba-git@127.0.0.1:alice/server.git HEAD:main",
                )
                settled()
                tip = forge.succeed("git -C /root/demo rev-parse HEAD").strip()
                assert refs(forge, directory, user="kajiba-git") == {**expected, "refs/heads/main": tip}
                assert events()[:-1] == saved
                assert json.loads(events()[-1]["payload"]) == {"ref": "refs/heads/main", "old": expected["refs/heads/main"], "new": tip}
                continue

            if key == "timeout":
                finished(key)
                assert "timed out" in forge.succeed(f"cat /root/{key}.out")
                assert forge.succeed(f"systemctl show -P MainPID {git_unit}").strip() == pid
                assert generation() == row["git_generation"]
                assert any(line.get("event") == "session-exited" and line.get("session") == row["git_session_id"] and line["status"] != 0 for line in journal(git_unit))
                forge.fail("pgrep -u kajiba-git -f 'git.*(fetch|remote-http)'")
            else:
                if key == "both":
                    crash(unit)
                crash(git_unit)
                finished(key)
                # Check death before a new generation can start, including transport helpers.
                forge.fail(f"kill -0 {child}")
                forge.fail("pgrep -u kajiba-git -f 'git.*(fetch|remote-http)'")
                assert pending() == [row] and events() == before
                forge.succeed(f"systemctl start {git_unit}")
                if key == "both":
                    forge.succeed(f"systemctl start {unit}")
                ready()
                assert generation() != row["git_generation"]
            forge.fail(f"kill -0 {child}")
            assert refs(forge, directory, user="kajiba-git") == {}
            recovered(before, row, [])
            forge.succeed(f"touch {control}/{key}.release")
            # Retry the same repository through real admission after reconciliation.
            forge.succeed(f"kajiba-server admin import alice/{key} {bundle}")
            retried = dict(row, correlation_id=events()[-1]["correlation_id"])
            recovered(before, retried, json.loads(row["updates"]))
            assert refs(forge, directory, user="kajiba-git") == expected

    validate_logs(forge, unit)
    validate_logs(forge, git_unit)
    validate_metrics(forge, metrics)
  '';
}
