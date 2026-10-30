# J4 task 18: the real CLI, declared credentials and both Git transports.
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  kajibaCliPackage,
  testLib,
}:
let
  # Public fixture credentials, never deployment secrets.
  pat = "kjb_" + pkgs.lib.concatStrings (pkgs.lib.replicate 43 "A");
  expiredPat = "kjb_" + pkgs.lib.concatStrings (pkgs.lib.replicate 43 "B");
  version = kajibaCliPackage.version;
  otherVersion =
    if builtins.elemAt (pkgs.lib.splitString "." version) 1 == "2" then "0.3.0" else "0.2.0";
  # Headers uses CARGO_PKG_VERSION: build another CLI without a product override hook.
  otherCli = kajibaCliPackage.overrideAttrs (old: {
    version = otherVersion;
    postPatch = (old.postPatch or "") + ''
      printf '%s\n' '${otherVersion}' > VERSION
      substituteInPlace Cargo.toml \
        --replace-fail 'version = "${version}"' 'version = "${otherVersion}"'
      sed -i '/^name = "kajiba-/ { n; s/^version = "${version}"$/version = "${otherVersion}"/; }' Cargo.lock
    '';
  });
in
pkgs.testers.runNixOSTest {
  name = "kajiba-cli";

  nodes.forge = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      http.listen = "0.0.0.0:8080";
      http.publicUrl = "https://forge.example.org";
      users.alice = {
        sshKeys = [ (builtins.readFile ./fixtures/ssh-reader-key.pub) ];
        tokens = {
          cli = {
            sha256 = builtins.hashString "sha256" pat;
            scopes = [ "repo:read" ];
          };
          expired = {
            sha256 = builtins.hashString "sha256" expiredPat;
            scopes = [ "repo:read" ];
            expires = "2000-01-01T00:00:00Z";
          };
        };
      };
      repos.demo = {
        owner = "alice";
        description = "CLI fixture";
      };
    };
    environment.systemPackages = [
      kajibaCliPackage
      pkgs.git
      pkgs.openssh
    ];
  };

  testScript = testLib + ''
    import json

    base = "http://forge:8080"
    bundle = "/var/lib/kajiba-fixtures/demo.bundle"
    cli = f"kajiba --url {base} --token-file /root/token"
    other_cli = f"${otherCli}/bin/kajiba --url {base} --token-file /root/token"


    def cli_refusal(command, reason):
        code, output = forge.execute(command + " >/tmp/cli.out 2>/tmp/cli.err")
        error = forge.succeed("cat /tmp/cli.err")
        assert code == 1 and output == "", (code, output, error)
        assert forge.succeed("cat /tmp/cli.out") == ""
        assert error.startswith("kajiba: ") and reason in error, error
        return error


    forge.wait_for_unit("kajiba-server.service")
    forge.wait_for_unit("kajiba-sshd.service")
    source = make_demo(forge, bundle)
    forge.succeed(
        f"kajiba-server admin import alice/demo {bundle}",
        "umask 077; printf '%s' '${pat}' > /root/token",
        "umask 077; printf '%s' '${expiredPat}' > /root/expired-token",
        "install -m 600 /dev/null /root/no-token",
    )

    with subtest("declared PAT lists and gets the declared repository in text and protojson"):
        assert forge.succeed(f"{cli} repo list") == "alice/demo  CLI fixture\n"
        listed = json.loads(forge.succeed(f"{cli} --json repo list"))
        assert len(listed) == 1, listed
        repo = listed[0]
        assert repo["owner"] == "alice" and repo["name"] == "demo", repo
        assert repo["description"] == "CLI fixture" and repo["visibility"] == "VISIBILITY_PUBLIC", repo
        text = forge.succeed(f"{cli} repo get alice/demo")
        assert text.startswith("alice/demo\n") and f"id: {repo['publicId']}\n" in text, text
        assert "description: CLI fixture\n" in text and "visibility: public\n" in text, text
        for address in ["alice/demo", repo["publicId"]]:
            assert json.loads(forge.succeed(f"{cli} --json repo get {address}")) == repo

    with subtest("smart HTTP and dedicated SSH clone the listed repository"):
        forge.succeed(
            "install -m 600 ${./fixtures/ssh-reader-key} /root/reader-key",
            "ssh-keyscan -p 2222 127.0.0.1 > /root/known_hosts",
            f"git clone {base}/alice/demo.git /root/http-clone",
        )
        ssh_command = "ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/known_hosts -o ConnectTimeout=5 -i /root/reader-key -p 2222"
        forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git clone kajiba-git@127.0.0.1:alice/demo.git /root/ssh-clone")
        for directory in ["/root/http-clone", "/root/ssh-clone"]:
            assert forge.succeed(f"git -C {directory} rev-parse HEAD").strip() == source["refs/heads/main"]
            assert head(forge, directory + "/.git") == "refs/heads/main"
            assert forge.succeed(f"cat {directory}/flake.nix") == DEMO_FLAKE
            assert forge.succeed(f"cat {directory}/README.md") == "main readme\n"

    with subtest("expired and absent credentials are refused"):
        cli_refusal(f"kajiba --url {base} --token-file /root/expired-token repo list", "CREDENTIAL_EXPIRED")
        # An empty file reaches server authentication; a missing file fails locally.
        cli_refusal(f"kajiba --url {base} --token-file /root/no-token repo list", "CREDENTIAL_INVALID")
        cli_refusal(f"kajiba --url {base} --token-file /root/missing-token repo list", "no token file")

    with subtest("another compiled 0.minor is refused and names both versions"):
        assert forge.succeed("kajiba --version").strip() == "kajiba ${version}"
        assert forge.succeed("${otherCli}/bin/kajiba --version").strip() == "kajiba ${otherVersion}"
        error = cli_refusal(f"{other_cli} repo list", "VERSION_INCOMPATIBLE")
        assert "client version ${otherVersion}" in error and "server version ${version}" in error, error

    validate_logs(forge, "kajiba-server.service")
    validate_logs(forge, "kajiba-git.service")
    logs = [json.loads(line) for line in forge.succeed("journalctl _SYSTEMD_UNIT=kajiba-server.service -o cat --no-pager").splitlines()]
    codes = [line.get("grpc_code") for line in logs if line.get("event") == "request"]
    assert codes.count("UNAUTHENTICATED") == 2, codes
    assert codes.count("FAILED_PRECONDITION") == 1, codes
    reasons = {line.get("reason") for line in logs if line.get("event") == "auth-refused"}
    assert {"CREDENTIAL_EXPIRED", "CREDENTIAL_INVALID"} <= reasons, reasons
  '';
}
