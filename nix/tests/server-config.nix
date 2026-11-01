# J1 task 29: the module's generated file passes `config check --files` with its secret file
# provisioned; an inline secret and an unknown field are refused by `config check`, naming the
# field; a missing token_file stops startup with a `config-invalid` line naming it (0015, 0019, 0022).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
let
  tokenFile = "/run/secrets/kajiba-github-token";
in
pkgs.testers.runNixOSTest {
  name = "kajiba-server-config";

  nodes.machine = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      ui.enable = false;
      http.publicUrl = "https://forge.example.org";
      mirror.credentials.github = { inherit tokenFile; };
    };
    users.groups.shared-secret.gid = 4242;
    systemd.services.kajiba-git.serviceConfig.SupplementaryGroups = [ "4242" ];
    # The mirror credential exists only at runtime, readable by the service user alone (0019).
    systemd.tmpfiles.settings."10-kajiba-test" = {
      "/run/secrets".d.mode = "0755";
      ${tokenFile}.f = {
        user = "kajiba-server";
        group = "kajiba-server";
        mode = "0400";
        argument = "DO-NOT-PRINT";
      };
    };
  };

  testScript = testLib + ''
    import json

    unit = "kajiba-server.service"
    config = "/etc/kajiba/server.toml"
    token_file = "${tokenFile}"


    def check(path, files=False):
        # Returns the exit status and the parsed stderr lines; the output must pass nisshi too.
        flag = "--files " if files else ""
        status, _ = machine.execute(f"runuser -u kajiba-server -- kajiba-server config check {flag}{path} >/tmp/check.out 2>/tmp/check.err")
        out = machine.succeed("cat /tmp/check.out")
        err = machine.succeed("cat /tmp/check.err")
        assert out == "", f"config check {flag}{path} wrote stdout: {out!r}"
        assert "DO-NOT-PRINT" not in err, f"config check {flag}{path} echoed a secret: {err!r}"
        validate_log_output(machine, "cat /tmp/check.err")
        return status, [json.loads(line) for line in err.splitlines()]


    def assert_refused(path, field):
        status, lines = check(path)
        assert status == 1, f"config check {path}: exit {status}, output {lines}"
        assert len(lines) == 1 and lines[0]["event"] == "config-invalid" and field in lines[0]["err"], f"config check {path}: expected one config-invalid naming {field!r}, got {lines}"


    def variant(path, transform):
        original = machine.succeed(f"cat {config}")
        changed = transform(original)
        assert changed != original, f"variant {path} left the generated file unchanged: {original!r}"
        machine.succeed(f"printf %s {shlex.quote(changed)} > {path}")
        return path


    with subtest("the service starts with the generated file and its provisioned secret"):
        machine.wait_for_unit(unit)
        state = machine.succeed(f"systemctl show -P ActiveState {unit}").strip()
        assert state == "active", f"ActiveState: {state!r}"

    with subtest("the generated file passes config check --files as the service user"):
        generated = machine.succeed(f"cat {config}")
        assert f'token_file = "{token_file}"' in generated, f"mirror token_file not in {config}: {generated!r}"
        machine.fail(f"runuser -u kajiba-git -- cat {token_file}")
        status, lines = check(config, files=True)
        assert status == 0 and lines == [], f"config check --files {config}: exit {status}, output {lines}"

    with subtest("server secrets reject fixed and numeric supplementary git-group reads"):
        git_pid = machine.succeed("systemctl show -P MainPID kajiba-git.service").strip()
        groups = machine.succeed(f"grep '^Groups:' /proc/{git_pid}/status").split()[1:]
        assert "4242" in groups, groups
        machine.succeed(f"chgrp kajiba-server {token_file}", f"chmod 0640 {token_file}")
        machine.fail(f"runuser -u kajiba-git -- cat {token_file}")
        status, lines = check(config, files=True)
        assert status == 0 and lines == [], (status, lines)
        for group, mode, reason in [
            ("kajiba-git", "0640", "readable by group kajiba-git"),
            ("shared-secret", "0640", "readable by a git-side group"),
            ("kajiba-server", "0660", "group-writable"),
            ("kajiba-server", "0601", "accessible by others"),
        ]:
            machine.succeed(f"chgrp {group} {token_file}", f"chmod {mode} {token_file}")
            if group == "kajiba-git":
                machine.succeed(f"runuser -u kajiba-git -- cat {token_file} >/dev/null")
            if group == "shared-secret":
                machine.succeed(f"systemd-run --wait --pipe --uid=kajiba-git --property=SupplementaryGroups=4242 cat {token_file} >/dev/null")
            status, lines = check(config, files=True)
            assert status == 1 and len(lines) == 1, (status, lines)
            assert lines[0]["event"] == "config-invalid" and reason in lines[0]["err"], lines
        machine.succeed(f"chgrp kajiba-server {token_file}", f"chmod 0400 {token_file}")

    with subtest("an inline secret is refused by config check, naming token_file"):
        inline = variant("/tmp/inline.toml", lambda text: text.replace(f'token_file = "{token_file}"', 'token = "DO-NOT-PRINT"'))
        assert_refused(inline, "token_file")

    with subtest("an unknown field is refused by config check, naming the field"):
        unknown = variant("/tmp/unknown.toml", lambda text: "colour = \"blue\"\n" + text)
        assert_refused(unknown, "colour")

    with subtest("a missing token_file stops startup with config-invalid naming the file"):
        # Stop listeners too while preparing the failure, then exercise normal activation.
        machine.succeed(
            f"systemctl stop {unit} kajiba-server.socket",
            f"rm {token_file}",
            f"systemctl reset-failed {unit} kajiba-server.socket",
            "journalctl --sync",
        )
        cursor = machine.succeed("journalctl -n 1 --show-cursor --no-pager").split("-- cursor: ")[-1].strip()
        # The readiness probe stops with the main process: each failed start must be prompt.
        status, out = machine.execute(f"timeout 60 systemctl start {unit}")
        assert status not in (0, 124), f"systemctl start: exit {status}, output {out!r}"
        machine.wait_until_succeeds(f"test \"$(systemctl show -P Result {unit})\" = start-limit-hit", timeout=60)
        state = machine.succeed(f"systemctl show -p ActiveState -p Result {unit}").split()
        assert state == ["ActiveState=failed", "Result=start-limit-hit"], f"after exhausting starts: {state}"
        # systemd propagates the service start limit to its triggering sockets, closing them.
        machine.wait_until_succeeds("test \"$(systemctl show -P Result kajiba-server.socket)\" = service-start-limit-hit")
        assert machine.succeed("systemctl show -P ActiveState kajiba-server.socket").strip() == "failed"
        machine.fail("curl -fsS --max-time 2 http://127.0.0.1:8080/readyz")
        machine.succeed("journalctl --sync")
        out = machine.succeed(f"journalctl --after-cursor={shlex.quote(cursor)} _SYSTEMD_UNIT={unit} -o json --no-pager")
        invocations = {}
        for entry in map(json.loads, out.splitlines()):
            invocations.setdefault(entry["_SYSTEMD_INVOCATION_ID"], []).append(json.loads(entry["MESSAGE"]))
        assert len(invocations) == 10, f"expected the module's ten start attempts: {invocations}"
        for invocation, lines in invocations.items():
            assert len(lines) == 1 and lines[0]["event"] == "config-invalid" and lines[0]["level"] == "error" and f"mirror.credentials.github.token_file ({token_file})" in lines[0]["err"], f"failed invocation {invocation}: expected one config-invalid naming the file, got {lines}"
        # Repairing configuration and explicitly restarting recovers the stopped listeners.
        machine.succeed(
            f"install -o kajiba-server -g kajiba-server -m 0400 /dev/null {token_file}",
            f"printf %s DO-NOT-PRINT > {token_file}",
            f"systemctl reset-failed {unit} kajiba-server.socket",
            f"systemctl start {unit}",
        )
        machine.wait_for_unit(unit)
        machine.succeed("curl -fsS --max-time 5 http://127.0.0.1:8080/readyz")
        validate_logs(machine, unit)
        validate_logs(machine, "kajiba-git.service")

    with subtest("invalid git configuration also exhausts starts and closes both sockets"):
        git_unit = "kajiba-git.service"
        machine.succeed(
            "systemctl stop kajiba-server.service kajiba-server.socket kajiba-git.service kajiba-git.socket kajiba-git-ssh.socket",
            f"mkdir -p /run/systemd/system/{git_unit}.d",
            f"printf '[Service]\\nExecStart=\\nExecStart=${kajibaGitPackage}/bin/kajiba-git serve --config /run/missing-git.toml\\n' > /run/systemd/system/{git_unit}.d/invalid.conf",
            "systemctl daemon-reload",
            f"systemctl reset-failed {git_unit} kajiba-git.socket kajiba-git-ssh.socket",
            "journalctl --sync",
        )
        cursor = machine.succeed("journalctl -n 1 --show-cursor --no-pager").split("-- cursor: ")[-1].strip()
        status, out = machine.execute(f"timeout 60 systemctl start {git_unit}")
        assert status not in (0, 124), f"systemctl start: exit {status}, output {out!r}"
        machine.wait_until_succeeds(f"test \"$(systemctl show -P Result {git_unit})\" = start-limit-hit", timeout=60)
        assert machine.succeed(f"systemctl show -P ActiveState {git_unit}").strip() == "failed"
        for socket in ["kajiba-git.socket", "kajiba-git-ssh.socket"]:
            machine.wait_until_succeeds(f"test \"$(systemctl show -P Result {socket})\" = service-start-limit-hit")
            assert machine.succeed(f"systemctl show -P ActiveState {socket}").strip() == "failed"
        machine.succeed("journalctl --sync")
        out = machine.succeed(f"journalctl --after-cursor={shlex.quote(cursor)} _SYSTEMD_UNIT={git_unit} -o json --no-pager")
        invocations = {}
        for entry in map(json.loads, out.splitlines()):
            invocations.setdefault(entry["_SYSTEMD_INVOCATION_ID"], []).append(json.loads(entry["MESSAGE"]))
        assert len(invocations) == 10, invocations
        for lines in invocations.values():
            assert len(lines) == 1 and lines[0]["event"] == "config-invalid" and "/run/missing-git.toml" in lines[0]["err"], lines
        validate_logs(machine, git_unit)
  '';
}
