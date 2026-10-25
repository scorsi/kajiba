# J2 task 15: configurations switched through specialisations rename a repository (redirect,
# same directory), archive it on removal (content in place, refused for git and /raw/, still
# answered and browsable by RepoService), refuse another key taking its names, unarchive it on re-declaration,
# and quarantine a stray directory (0012, 0021, 0026, 0032).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
let
  inherit (pkgs) lib;
  stateDir = "/var/lib/kajiba-server";
  gitStateDir = "/var/lib/kajiba-git";
  repo = {
    owner = "alice";
    description = "Demo repository";
  };
  repos = value: { services.kajiba-server.repos = lib.mkForce value; };
in
pkgs.testers.runNixOSTest {
  name = "kajiba-repo-lifecycle";

  nodes.forge = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      inherit stateDir;
      http.listen = "0.0.0.0:8080";
      http.publicUrl = "https://forge.example.org";
      http.reflection = true;
      users.alice = { };
      repos.demo = repo;
    };
    specialisation = {
      added.configuration = repos {
        demo = repo;
        extra = repo;
      };
      renamed.configuration = repos {
        demo = repo // {
          name = "demo2";
          previousNames = [ "alice/demo" ];
        };
      };
      removed.configuration = repos { };
      taken-name.configuration = repos {
        other = repo // {
          name = "demo2";
        };
      };
      taken-old-name.configuration = repos {
        other = repo // {
          name = "demo";
        };
      };
    };
    environment.systemPackages = [
      pkgs.curl
      pkgs.git
      pkgs.grpcurl
    ];
  };

  testScript =
    { nodes, ... }:
    testLib
    + ''
      import base64
      import json

      unit = "kajiba-server.service"
      base = "http://forge:8080"
      bundle = "/var/lib/kajiba-fixtures/demo.bundle"
      toplevel = "${nodes.forge.system.build.toplevel}"
      orphans = "${gitStateDir}/tmp/orphans"


      def http(path):
          # (status code, body, Location header) of a GET, redirects not followed.
          out = forge.succeed(f"curl -s -o /tmp/body -w '%{{http_code}} %header{{location}}' {shlex.quote(base + path)}")
          code, _, location = out.partition(" ")
          return int(code), forge.succeed("cat /tmp/body"), location


      def get_repo(**request):
          out = forge.succeed(f"grpcurl -plaintext -d {shlex.quote(json.dumps(request))} forge:8080 kajiba.v1.RepoService/GetRepo")
          repo = json.loads(out)["repo"]
          # Proto3 JSON omits false booleans.
          return repo.get("publicId", repo.get("public_id")), f"{repo['owner']}/{repo['name']}", repo.get("archived", False)


      def invocation_events():
          forge.succeed("journalctl --sync")
          invocation = forge.succeed(f"systemctl show -P InvocationID {unit}").strip()
          out = forge.succeed(f"journalctl _SYSTEMD_INVOCATION_ID={invocation} _SYSTEMD_UNIT={unit} -o cat --no-pager")
          return [json.loads(line) for line in out.splitlines()]


      def assert_ready():
          forge.wait_for_unit(unit)
          state = forge.succeed(f"systemctl show -P ActiveState {unit}").strip()
          assert state == "active", f"ActiveState: {state!r}"


      def switch(specialisation, check=True):
          # Stopped first, so the start below is the one invocation that reconciles this configuration.
          path = toplevel if specialisation is None else f"{toplevel}/specialisation/{specialisation}"
          forge.succeed(f"systemctl stop {unit}", f"systemctl reset-failed {unit} || true")
          # A refused configuration may already fail inside the switch if it starts the unit.
          status, out = forge.execute(f"{path}/bin/switch-to-configuration test")
          assert status == 0 or not check, f"switch to {specialisation}: exit {status}, output {out!r}"


      def start():
          forge.succeed(f"systemctl start {unit}")
          assert_ready()
          return invocation_events()


      def clone(name, target):
          forge.succeed(f"rm -rf {target}", f"git clone --mirror {base}/{name}.git {target}")
          return refs(forge, target), head(forge, target)


      def git_dir(public_id):
          return f"${gitStateDir}/repos/{public_id}.git"


      def inode(path):
          return forge.succeed(f"stat -c %i {path}").strip()


      def no_orphans():
          forge.succeed(f"test ! -e {orphans} || test -z \"$(ls -A {orphans})\"")


      def refused_start(specialisation, name):
          switch(specialisation, check=False)
          status, out = forge.execute(f"timeout 60 systemctl start {unit}")
          assert status not in (0, 124), f"systemctl start: exit {status}, output {out!r}"
          state = forge.succeed(f"systemctl show -p ActiveState -p Result {unit}").split()
          assert state == ["ActiveState=failed", "Result=exit-code"], f"after the refused start: {state}"
          fatal = [e for e in invocation_events() if e["event"] == "fatal"]
          assert len(fatal) == 1, f"fatal lines: {fatal}"
          err = fatal[0]["err"]
          assert "repos.other" in err and "repos.demo" in err and name in err, f"refusal must name both keys and {name}: {err!r}"


      # A refused start must fail once, not loop through Restart=on-failure.
      forge.succeed(
          f"mkdir -p /run/systemd/system/{unit}.d",
          f"printf '[Service]\\nRestart=no\\n' > /run/systemd/system/{unit}.d/no-restart.conf",
          "systemctl daemon-reload",
      )
      assert_ready()

      with subtest("alice/demo is imported and clonable"):
          source = make_demo(forge, bundle)
          forge.succeed(f"kajiba-server admin import alice/demo {bundle}")
          original = clone("alice/demo", "/root/original.git")
          assert original == (source, "refs/heads/main"), f"clone {original} != source {source}"
          public_id, name, archived = get_repo(name="alice/demo")
          assert (name, archived) == ("alice/demo", False), f"GetRepo: {public_id} {name} {archived}"
          directory = inode(git_dir(public_id))

      with subtest("startup needing a new repository fails explicitly while the git side is unavailable"):
          # Mask prevents the server's Wants from restarting the daemon for this start.
          forge.succeed("systemctl mask --runtime --now kajiba-git.service")
          switch("added", check=False)
          status, out = forge.execute(f"timeout 60 systemctl start {unit}")
          assert status not in (0, 124), f"start without git: {status}, {out}"
          fatal = [e for e in invocation_events() if e["event"] == "fatal"]
          assert len(fatal) == 1 and "GIT_SIDE_UNAVAILABLE" in fatal[0]["err"], fatal
          forge.succeed("systemctl unmask --runtime kajiba-git.service", "systemctl start kajiba-git.service")
          start()
          assert get_repo(name="alice/extra")[1:] == ("alice/extra", False)
          assert clone("alice/demo", "/root/recovered.git") == original

      with subtest("rename: the old name clones and redirects, same row and directory"):
          switch("renamed")
          events = start()
          renamed = [e for e in events if e["event"] == "repo-renamed"]
          assert len(renamed) == 1 and renamed[0]["repo"] == "alice/demo2" and renamed[0]["from"] == "alice/demo", f"repo-renamed: {renamed}"
          assert get_repo(name="alice/demo") == (public_id, "alice/demo2", False)
          assert get_repo(name="alice/demo2") == (public_id, "alice/demo2", False)
          assert clone("alice/demo", "/root/old.git") == original
          assert clone("alice/demo2", "/root/new.git") == original
          code, _, location = http("/alice/demo")
          assert (code, location) == (301, "/alice/demo2"), f"UI route of the old name: {code} {location!r}"
          assert inode(git_dir(public_id)) == directory, "the repository directory changed on rename"

      with subtest("removal archives: content in place, git and /raw/ refused, RepoService still answers"):
          switch("removed")
          events = start()
          archived = [e for e in events if e["event"] == "repo-archived"]
          assert len(archived) == 1 and archived[0]["repo"] == "alice/demo2" and archived[0]["level"] == "warn", f"repo-archived: {archived}"
          assert inode(git_dir(public_id)) == directory, "the archived repository directory moved"
          no_orphans()
          for name in ["alice/demo2", "alice/demo"]:
              forge.fail(f"git clone {base}/{name}.git /root/refused")
              code, body, _ = http(f"/{name}.git/info/refs?service=git-upload-pack")
              assert code == 403 and "archived" in body, f"info/refs of {name}: {code} {body!r}"
              code, body, _ = http(f"/raw/{name}/main/flake.nix")
              assert code == 403 and "archived" in body, f"/raw/ of {name}: {code} {body!r}"
          for request in [{"publicId": public_id}, {"name": "alice/demo2"}, {"name": "alice/demo"}]:
              assert get_repo(**request) == (public_id, "alice/demo2", True), f"GetRepo {request}: {get_repo(**request)}"
          # The content is kept in place and browsable through the API, by both names.
          assert refs(forge, git_dir(public_id), user="kajiba-git") == source
          for name in ["alice/demo2", "alice/demo"]:
              request = json.dumps({"repo": name, "revision": "main", "path": "flake.nix"})
              out = forge.succeed(f"grpcurl -plaintext -d {shlex.quote(request)} forge:8080 kajiba.v1.RepoService/GetBlob")
              content = base64.b64decode(json.loads(out)["content"]).decode()
              assert content == DEMO_FLAKE, f"GetBlob flake.nix of the archived {name}: {content!r}"
          code, _, location = http("/alice/demo")
          assert (code, location) == (301, "/alice/demo2"), f"UI route of the old name after archival: {code} {location!r}"

      with subtest("another key with the archived name is refused at startup, naming both keys"):
          refused_start("taken-name", "alice/demo2")

      with subtest("another key with the archived repository's old name is refused at startup"):
          refused_start("taken-old-name", "alice/demo")

      with subtest("re-declaring the original key unarchives the same repository, no import"):
          switch("renamed")
          events = start()
          unarchived = [e for e in events if e["event"] == "repo-unarchived"]
          assert len(unarchived) == 1 and unarchived[0]["repo"] == "alice/demo2", f"repo-unarchived: {unarchived}"
          assert get_repo(publicId=public_id) == (public_id, "alice/demo2", False)
          assert clone("alice/demo2", "/root/back.git") == original
          assert clone("alice/demo", "/root/back-old.git") == original
          assert http("/raw/alice/demo2/main/flake.nix")[:2] == (200, DEMO_FLAKE)
          assert inode(git_dir(public_id)) == directory
          no_orphans()

      with subtest("a stray directory under repos/ is quarantined in tmp/orphans/"):
          forge.succeed(
              "install -d -o kajiba-git -g kajiba-git -m 0750 ${gitStateDir}/repos/01ARZ3NDEKTSV4RRFFQ69G5FAV.git",
              "install -o kajiba-git -g kajiba-git -m 0640 /dev/null ${gitStateDir}/repos/01ARZ3NDEKTSV4RRFFQ69G5FAV.git/marker",
              f"systemctl stop {unit}",
          )
          events = start()
          forge.succeed("journalctl --sync")
          journal = forge.succeed("journalctl _SYSTEMD_UNIT=kajiba-git.service -o cat --no-pager")
          orphaned = [json.loads(line) for line in journal.splitlines() if json.loads(line)["event"] == "repo-quarantined"]
          assert len(orphaned) == 1 and orphaned[0]["destination"] == f"{orphans}/01ARZ3NDEKTSV4RRFFQ69G5FAV.git", f"repo-orphaned: {orphaned}"
          forge.succeed(f"test -f {orphans}/01ARZ3NDEKTSV4RRFFQ69G5FAV.git/marker", "test ! -e ${gitStateDir}/repos/01ARZ3NDEKTSV4RRFFQ69G5FAV.git")
          assert clone("alice/demo2", "/root/after.git") == original

      with subtest("every service log line validates against the nisshi schema"):
          validate_logs(forge, unit)
          validate_logs(forge, "kajiba-git.service")
    '';
}
