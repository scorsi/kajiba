# J2 task 14: a declared repository filled by `admin import` of a bundle is clonable over
# smart HTTP with the same refs and HEAD, usable as a Nix flake input and readable through
# /raw/; an undeclared repository is a clean 404 and a second import is refused
# (0015, 0021, 0022, 0026).
{
  pkgs,
  kajibaServerPackage,
  testLib,
}:
pkgs.testers.runNixOSTest {
  name = "kajiba-git-http";

  nodes.forge = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      # Clone URLs name the host: listen beyond loopback.
      http.listen = "0.0.0.0:8080";
      http.publicUrl = "https://forge.example.org";
      users.alice = { };
      repos.demo = {
        owner = "alice";
        description = "Demo repository";
      };
    };
    nix.settings = {
      experimental-features = [
        "nix-command"
        "flakes"
      ];
      # No network beyond the forge: no registry download, no substituter.
      flake-registry = "";
      substituters = pkgs.lib.mkForce [ ];
    };
    environment.systemPackages = [
      pkgs.curl
      pkgs.git
    ];
  };

  testScript = testLib + ''
    import json

    unit = "kajiba-server.service"
    base = "http://forge:8080"
    bundle = "/var/lib/kajiba-fixtures/demo.bundle"


    def http(path):
        # (status code, body) of a GET.
        out = forge.succeed(f"curl -s -o /tmp/body -w '%{{http_code}}' {shlex.quote(base + path)}")
        return int(out), forge.succeed("cat /tmp/body")


    def import_demo():
        status, out = forge.execute(f"kajiba-server admin import alice/demo {bundle} 2>/tmp/import.err")
        err = forge.succeed("cat /tmp/import.err")
        return status, out, err


    forge.wait_for_unit(unit)
    # systemd reaches active only once ExecStartPost's /readyz probe exited 0.
    state = forge.succeed(f"systemctl show -P ActiveState {unit}").strip()
    assert state == "active", f"ActiveState: {state!r}"

    with subtest("admin import of a bundle fills the declared repository"):
        source = make_demo(forge, bundle)
        assert set(source) == {"refs/heads/main", "refs/heads/feature/x", "refs/tags/v1"}, f"fixture refs: {source}"
        status, out, err = import_demo()
        assert status == 0 and out == "", f"import: exit {status}, stdout {out!r}, stderr {err!r}"

    with subtest("git clone over smart HTTP has the same refs and HEAD"):
        forge.succeed(f"git clone --mirror {base}/alice/demo.git /root/clone.git")
        cloned = refs(forge, "/root/clone.git")
        assert cloned == source, f"cloned refs {cloned} != source {source}"
        assert head(forge, "/root/clone.git") == "refs/heads/main"
        # A working-tree clone checks out the default branch's files.
        forge.succeed(f"git clone {base}/alice/demo.git /root/work")
        assert forge.succeed("cat /root/work/flake.nix") == DEMO_FLAKE

    with subtest("nix flake prefetch git+http succeeds"):
        out = json.loads(forge.succeed(f"nix flake prefetch --json git+{base}/alice/demo.git"))
        assert forge.succeed(f"cat {out['storePath']}/flake.nix") == DEMO_FLAKE, f"prefetch: {out}"
        assert out["locked"]["rev"] == source["refs/heads/main"], f"prefetch locked {out['locked']}, main is {source['refs/heads/main']}"

    with subtest("/raw/ serves files at a branch, including a branch name with a slash"):
        assert http("/raw/alice/demo/main/flake.nix") == (200, DEMO_FLAKE)
        assert http("/raw/alice/demo/main/README.md") == (200, "main readme\n")
        assert http("/raw/alice/demo/feature/x/README.md") == (200, "feature readme\n")

    with subtest("an undeclared repository is a clean 404"):
        forge.fail(f"git clone {base}/alice/nope.git /root/nope")
        forge.fail("test -e /root/nope")
        assert http("/alice/nope.git/info/refs?service=git-upload-pack")[0] == 404
        assert http("/raw/alice/nope/main/flake.nix")[0] == 404

    with subtest("a second import into alice/demo is refused and changes nothing"):
        status, out, err = import_demo()
        lines = [json.loads(line) for line in err.splitlines()]
        assert status == 1 and out == "", f"second import: exit {status}, stdout {out!r}, stderr {err!r}"
        assert len(lines) == 1 and lines[0]["event"] == "fatal" and "already has refs" in lines[0]["err"], f"second import stderr: {lines}"
        validate_log_output(forge, "cat /tmp/import.err")
        forge.succeed(f"git clone --mirror {base}/alice/demo.git /root/again.git")
        assert refs(forge, "/root/again.git") == source

    with subtest("the import is logged once and every service line validates against nisshi"):
        forge.succeed("journalctl --sync")
        journal = forge.succeed(f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager")
        imported = [json.loads(line) for line in journal.splitlines() if '"repo-imported"' in line]
        assert len(imported) == 1 and imported[0]["repo"] == "alice/demo" and imported[0]["refs"] == 3, f"repo-imported lines: {imported}"
        validate_logs(forge, unit)
  '';
}
