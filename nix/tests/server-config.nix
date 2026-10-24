# J1 task 29: the module's generated file passes `config check --files` with its secret file
# provisioned; an inline secret and an unknown field are refused by `config check`, naming the
# field; a missing token_file stops startup with a `config-invalid` line naming it (0015, 0019, 0022).
{
  pkgs,
  kajibaServerPackage,
  testLib,
}:
let
  tokenFile = "/run/secrets/kajiba-runner-build-1";
in
pkgs.testers.runNixOSTest {
  name = "kajiba-server-config";

  nodes.machine = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      http.publicUrl = "https://forge.example.org";
      runners.build-1 = { inherit tokenFile; };
    };
    # The runner secret exists only at runtime, readable by the service user alone (0019).
    systemd.tmpfiles.settings."10-kajiba-test" = {
      "/run/secrets".d.mode = "0755";
      ${tokenFile}.f = {
        user = "kajiba";
        group = "kajiba";
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
        status, _ = machine.execute(f"runuser -u kajiba -- kajiba-server config check {flag}{path} >/tmp/check.out 2>/tmp/check.err")
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
        status, lines = check(config, files=True)
        assert status == 0 and lines == [], f"config check --files {config}: exit {status}, output {lines}"

    with subtest("an inline secret is refused by config check, naming token_file"):
        inline = variant("/tmp/inline.toml", lambda text: text.replace(f'token_file = "{token_file}"', 'token = "DO-NOT-PRINT"'))
        assert_refused(inline, "token_file")

    with subtest("an unknown field is refused by config check, naming the field"):
        unknown = variant("/tmp/unknown.toml", lambda text: "colour = \"blue\"\n" + text)
        assert_refused(unknown, "colour")

    with subtest("a missing token_file stops startup with config-invalid naming the file"):
        machine.succeed(f"systemctl stop {unit}", f"rm {token_file}")
        machine.fail(f"systemctl start {unit}")
        # Restart=on-failure retries until the start limit: the unit ends failed, never active.
        machine.wait_until_succeeds(f"systemctl show -P ActiveState {unit} | grep -x failed", timeout=60)
        result = machine.succeed(f"systemctl show -p ActiveState -p Result {unit}")
        print(f"after the missing token_file: {result!r}")
        machine.succeed("journalctl --sync")
        out = machine.succeed(f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager")
        invalid = [json.loads(line) for line in out.splitlines() if '"config-invalid"' in line]
        # Earlier config check runs were outside the unit, so every line here comes from this phase.
        assert invalid, f"no config-invalid line in the unit journal: {out}"
        for line in invalid:
            assert line["level"] == "error" and f"runners.build-1.token_file ({token_file})" in line["err"], f"config-invalid line does not name the file: {line}"
        validate_logs(machine, unit)
  '';
}
