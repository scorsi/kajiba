# J1 task 31: root gets an enrollment link over the local socket, other users are refused,
# a second link replaces the first, and neither token reaches the journal (0003, 0014, 0015, 0022).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
let
  stateDir = "/var/lib/kajiba-server";
in
pkgs.testers.runNixOSTest {
  name = "kajiba-server-enroll";

  nodes.machine = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      inherit stateDir;
      http.publicUrl = "https://forge.example.org";
      users.alice.isAdmin = true;
    };
    # Neither root nor in either service group.
    users.users.bob.isNormalUser = true;
    environment.systemPackages = [ pkgs.sqlite ];
  };

  testScript = testLib + ''
    import hashlib
    import json
    import re

    unit = "kajiba-server.service"
    db = "${stateDir}/db/kajiba.sqlite"
    socket = "/run/kajiba/local.sock"
    link = re.compile(r"https://forge\.example\.org/auth/enroll#([0-9a-f]{64})")


    def enroll(user="root"):
        out = machine.succeed(f"runuser -u {user} -- kajiba-server admin enroll alice")
        lines = out.splitlines()
        assert len(lines) == 1, f"expected exactly one line, got {out!r}"
        match = link.fullmatch(lines[0])
        assert match, f"not an enrollment link: {lines[0]!r}"
        return match.group(1)


    def refused(command):
        # Exit status, stdout and stderr in one shot; stderr must be one nisshi fatal line.
        status, out = machine.execute(f"{command} 2>/tmp/enroll.err")
        err = machine.succeed("cat /tmp/enroll.err")
        assert status != 0 and out == "", f"{command}: status {status}, stdout {out!r}, stderr {err!r}"
        lines = [json.loads(line) for line in err.splitlines()]
        assert len(lines) == 1 and lines[0]["event"] == "fatal" and lines[0]["err"].startswith("admin enroll: "), f"{command}: stderr {lines}"
        validate_log_output(machine, "cat /tmp/enroll.err")
        return lines[0]["err"]


    def stored_hashes():
        query = "SELECT lower(hex(t.token_hash)) FROM enrollment_token t JOIN user u ON u.id = t.user_id WHERE u.name = 'alice'"
        return machine.succeed(f"sqlite3 {db} {shlex.quote(query)}").split()


    def sha256(token):
        return hashlib.sha256(token.encode()).hexdigest()


    machine.wait_for_unit(unit)
    # systemd reaches active only once ExecStartPost's /readyz probe exited 0.
    state = machine.succeed(f"systemctl show -P ActiveState {unit}").strip()
    assert state == "active", f"ActiveState: {state!r}"

    with subtest("root gets exactly one link with a 64-hex token"):
        first = enroll()

    with subtest("a second link replaces the first in the database"):
        second = enroll()
        assert second != first, f"same token twice: {first}"
        hashes = stored_hashes()
        assert hashes == [sha256(second)], f"stored {hashes}, want only sha256(second) {sha256(second)} (sha256(first) {sha256(first)})"

    with subtest("the runtime directory is traversable but the admin socket is private"):
        machine.succeed(f"runuser -u bob -- test -e {socket}")
        machine.fail(f"runuser -u bob -- test -w {socket}")
        machine.fail(f"runuser -u kajiba-git -- test -w {socket}")
        print(refused("runuser -u kajiba-git -- kajiba-server admin enroll alice"))
        print(refused("runuser -u bob -- kajiba-server admin enroll alice"))

    with subtest("with the socket opened to everyone, the peer credential check still refuses bob and the git user"):
        machine.succeed(f"chmod 0755 /run/kajiba && chmod 0666 {socket}")
        machine.succeed(f"runuser -u bob -- test -r {socket} -a -w {socket}")
        print(refused("runuser -u bob -- kajiba-server admin enroll alice"))
        print(refused("runuser -u kajiba-git -- kajiba-server admin enroll alice"))
        assert stored_hashes() == [sha256(second)], f"bob's attempt changed the token: {stored_hashes()}"
        # The renamed server user is also an admitted peer.
        third = enroll("kajiba-server")
        assert stored_hashes() == [sha256(third)], f"stored {stored_hashes()} after third link"

    with subtest("an unknown user is refused with a clear error"):
        err = refused("kajiba-server admin enroll mallory")
        assert err == "admin enroll: unknown user mallory", f"err: {err!r}"

    with subtest("enrollment-issued lines never carry a token and validate against nisshi"):
        machine.succeed("journalctl --sync")
        journal = machine.succeed(f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager")
        issued = [json.loads(line) for line in journal.splitlines() if '"enrollment-issued"' in line]
        assert len(issued) == 3 and all(e["event"] == "enrollment-issued" and e["user"] == "alice" for e in issued), f"enrollment-issued lines: {issued}"
        for token in [first, second, third]:
            assert token not in journal, f"token {token} found in the journal"
        validate_logs(machine, unit)
        validate_logs(machine, "kajiba-git.service")
  '';
}
