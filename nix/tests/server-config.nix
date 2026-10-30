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
  tokenFile = "/run/secrets/kajiba-runner-build-1";
in
pkgs.testers.runNixOSTest {
  name = "kajiba-server-config";

  nodes.machine = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      http.publicUrl = "https://forge.example.org";
      runners.build-1 = {
        inherit tokenFile;
        capabilities = {
          arch = "x86_64";
          os = "linux";
        };
      };
    };
    users.groups.shared-secret.gid = 4242;
    systemd.services.kajiba-git.serviceConfig.SupplementaryGroups = [ "4242" ];
    # The runner secret exists only at runtime, readable by the service user alone (0019).
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
        assert f'token_file = "{token_file}"' in generated, f"runner token_file not in {config}: {generated!r}"
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
        machine.succeed(f"systemctl stop {unit}", f"rm {token_file}")
        # Runtime drop-in: one start attempt, no auto-restart, so the outcome needs no timing.
        machine.succeed(
            f"mkdir -p /run/systemd/system/{unit}.d",
            f"printf '[Service]\\nRestart=no\\n' > /run/systemd/system/{unit}.d/no-restart.conf",
            "systemctl daemon-reload",
        )
        # The readiness probe stops with the main process: the start job fails at once,
        # not after TimeoutStartSec (timeout exits 124).
        status, out = machine.execute(f"timeout 60 systemctl start {unit}")
        assert status not in (0, 124), f"systemctl start: exit {status}, output {out!r}"
        state = machine.succeed(f"systemctl show -p ActiveState -p Result {unit}").split()
        assert state == ["ActiveState=failed", "Result=exit-code"], f"after the failed start: {state}"
        machine.succeed("journalctl --sync")
        invocation = machine.succeed(f"systemctl show -P InvocationID {unit}").strip()
        out = machine.succeed(f"journalctl _SYSTEMD_INVOCATION_ID={invocation} _SYSTEMD_UNIT={unit} -o cat --no-pager")
        lines = [json.loads(line) for line in out.splitlines()]
        assert len(lines) == 1 and lines[0]["event"] == "config-invalid" and lines[0]["level"] == "error" and f"runners.build-1.token_file ({token_file})" in lines[0]["err"], f"failed invocation {invocation}: expected one config-invalid naming the file, got {lines}"
        validate_logs(machine, unit)
        validate_logs(machine, "kajiba-git.service")
  '';
}
