# The shared setup of the `nixos-ci-*` tests (J6 task 20, runner mTLS task 8): forge behind an
# nginx front relaying `runners.forge` as raw TCP by SNI to the runner listener (a stand-in for
# the reverse proxy) and any other name to the TLS front of a test CA, a declared runner by SVID,
# the fixture flake and the Python helpers. Each test passes its name and its subtests; the logs
# and metrics of every unit are validated at the end (0124, 0125, 0126).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  kajibaRunnerPackage,
  kajibaCliPackage,
  testLib,
}:
{
  name,
  # The test's subtests, after the helpers and the setup.
  script,
  # Builds with shortened constants, for the tests that wait on leases and timeouts.
  serverPackage ? kajibaServerPackage,
  runnerPackage ? kajibaRunnerPackage,
  # A NixOS module merged into the forge node.
  forgeConfig ? { },
}:
let
  inherit (pkgs) lib;
  inherit (pkgs.stdenv.hostPlatform) system;
  # Public fixture credentials, never deployment secrets.
  writerPat = "kjb_" + lib.concatStrings (lib.replicate 43 "W");
  readerPat = "kjb_" + lib.concatStrings (lib.replicate 43 "R");
  svid = import ./svid.nix { inherit pkgs; };
  runnerId = svid.id "runner" "kajiba-runner";
  # The HTTP front's own test CA and certificate (SAN `forge`), for the CLI and git.
  tls = pkgs.runCommand "kajiba-test-tls" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir "$out"
    cd "$out"
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
      -subj "/CN=kajiba test CA" -keyout ca.key -out ca.pem
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
      -subj "/CN=forge" -keyout forge.key -out forge.csr
    printf '%s\n' subjectAltName=DNS:forge basicConstraints=CA:FALSE \
      extendedKeyUsage=serverAuth keyUsage=digitalSignature > forge.ext
    openssl x509 -req -in forge.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 3650 \
      -extfile forge.ext -out forge.pem
  '';
in
pkgs.testers.runNixOSTest {
  name = "kajiba-${name}";

  nodes.forge = {
    imports = [
      ../nixosModules/server.nix
      (svid.install "kajiba-server" "forge")
      forgeConfig
    ];
    virtualisation.memorySize = 2048;
    services.kajiba-git.package = kajibaGitPackage;
    services.kajiba-server = {
      enable = true;
      package = serverPackage;
      ui.enable = false;
      http.publicUrl = "https://forge";
      metrics.listen = "127.0.0.1:9431";
      users = {
        alice = { };
        writer = {
          sshKeys = [ (builtins.readFile ./fixtures/ssh-writer-key.pub) ];
          signingKeys = [ (builtins.readFile ./fixtures/ssh-signing-key.pub) ];
          tokens.ci = {
            sha256 = builtins.hashString "sha256" writerPat;
            scopes = [
              "repo:read"
              "ci:trigger"
            ];
          };
        };
        reader.tokens.ci = {
          sha256 = builtins.hashString "sha256" readerPat;
          scopes = [
            "repo:read"
            "ci:trigger"
          ];
        };
      };
      runnerListener = {
        listen = "127.0.0.1:8443";
        trustDomain = "example.org";
      }
      // svid.files "kajiba-server";
      runners.build-1 = {
        spiffeId = runnerId;
        capabilities = {
          arch = pkgs.stdenv.hostPlatform.parsed.cpu.name;
          os = "linux";
          maxJobs = 2;
        };
      };
      repos.demo = {
        owner = "alice";
        description = "CI fixture";
        ci = "push";
        permissions = {
          writer = "write";
          reader = "read";
        };
      };
      # Short enough for `admin retention` to delete the first run's log within the test.
      ci.logRetention = "1m";
    };
    # The reverse proxy: on 443, `runners.forge` is relayed as raw TCP to the runner listener
    # (TLS end to end between runner and forge), any other name to the TLS front. The front:
    # gRPC (HTTP/2) to the server, long-lived streams kept open, request bodies unbounded; one
    # access-log line per request with its size.
    services.nginx = {
      streamConfig = ''
        map $ssl_preread_server_name $kajiba_upstream {
          runners.forge 127.0.0.1:8443;
          default 127.0.0.1:4443;
        }
        server {
          listen 443;
          listen [::]:443;
          ssl_preread on;
          proxy_pass $kajiba_upstream;
          proxy_timeout 1h;
        }
      '';
      enable = true;
      commonHttpConfig = ''
        log_format kajiba '$remote_addr $request_uri $status $body_bytes_sent';
      '';
      virtualHosts.forge = {
        listen = [
          {
            addr = "127.0.0.1";
            port = 4443;
            ssl = true;
          }
        ];
        onlySSL = true;
        http2 = true;
        sslCertificate = "${tls}/forge.pem";
        sslCertificateKey = "${tls}/forge.key";
        extraConfig = ''
          access_log /var/log/nginx/kajiba.log kajiba;
          client_max_body_size 0;
          client_body_timeout 1h;
          grpc_read_timeout 1h;
          grpc_send_timeout 1h;
        '';
        locations."/kajiba.".extraConfig = "grpc_pass grpc://127.0.0.1:8080;";
        locations."/".proxyPass = "http://127.0.0.1:8080";
      };
    };
    networking.firewall.allowedTCPPorts = [ 443 ];
    networking.hosts."127.0.0.1" = [ "runners.forge" ];
    security.pki.certificateFiles = [ "${tls}/ca.pem" ];
    environment.systemPackages = [
      kajibaCliPackage
      pkgs.git
      pkgs.openssh
      pkgs.sqlite
      pkgs.curl
      pkgs.grpcurl
    ];
  };

  nodes.runner =
    { nodes, ... }:
    {
      imports = [
        ../nixosModules/runner.nix
        (svid.install "kajiba-runner" "runner")
      ];
      virtualisation.memorySize = 2048;
      services.kajiba-runner = {
        enable = true;
        package = runnerPackage;
        name = "build-1";
        url = "https://runners.forge";
        # The bundle only: the system store is not trusted (0124).
        tls = {
          trustDomain = "example.org";
          inherit (svid) serverId;
        }
        // svid.files "kajiba-runner";
        maxJobs = 2;
        metrics.listen = "127.0.0.1:9432";
      };
      networking.hosts.${nodes.forge.networking.primaryIPAddress} = [ "runners.forge" ];
      nix.settings = {
        experimental-features = [
          "nix-command"
          "flakes"
        ];
        # No network: never wait on a binary cache.
        substituters = lib.mkForce [ ];
        # The fixture's closure: /bin/sh (busybox, Nix's default) and coreutils for `sleep`,
        # already in this system's store, so nothing is fetched.
        extra-sandbox-paths = [ "/tools=${pkgs.coreutils}/bin" ];
      };
      environment.systemPackages = [
        runnerPackage
        pkgs.git
      ];
    };

  testScript =
    testLib
    + ''
      ${svid.reissue}
      import base64
      import json
      import re

      db = "/var/lib/kajiba-server/db/kajiba.sqlite"
      logs_dir = "/var/lib/kajiba-server/ci/logs"
      metrics_url = "http://127.0.0.1:9431/metrics"
      writer_cli = "kajiba --token-file /root/writer-token"
      reader_cli = "kajiba --token-file /root/reader-token"
      ssh_command = "ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/known_hosts -o ConnectTimeout=5 -i /root/writer-key -p 2222"
      proto = "${../../proto}"
      manual = "/var/lib/kajiba-runner-manual"

      # Fixture flake without inputs: plain derivations built by /bin/sh in the sandbox. `salt`
      # changes with every commit so each run builds its checks afresh.
      FLAKE = """{
        outputs = _:
          let
            check = name: script: derivation {
              inherit name;
              system = "${system}";
              builder = "/bin/sh";
              args = [ "-c" script ];
              salt = builtins.readFile ./salt;
            };
          in
          {
            checks."${system}" = {
              pass = check "pass" "echo pass-ok; echo > $out";
      EXTRA      };
          };
      }
      """
      LOCK = '{"nodes":{"root":{}},"root":"root","version":7}\n'


      def sql(query):
          return forge.succeed(f"sqlite3 {db} {shlex.quote(query)}").strip()


      def unit_lines(machine, unit):
          machine.succeed("journalctl --sync")
          out = machine.succeed(f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager")
          return [json.loads(line) for line in out.splitlines()]


      def metric(machine, url, series):
          lines = machine.succeed(f"curl -fsS {url}").splitlines()
          return float(next(line.split()[-1] for line in lines if line.startswith(series + " ")))


      def commit(extra, message, sign=True):
          text = FLAKE.replace("EXTRA", extra)
          forge.succeed(
              f"printf %s {shlex.quote(text)} > /root/work/flake.nix",
              f"printf %s {shlex.quote(LOCK)} > /root/work/flake.lock",
              f"printf %s {shlex.quote(message)} > /root/work/salt",
              "git -C /root/work add -A",
              f"git -C /root/work commit -q {'-S' if sign else '--no-gpg-sign'} -m {shlex.quote(message)}",
          )
          return forge.succeed("git -C /root/work rev-parse HEAD").strip()


      def push(sha):
          out = forge.succeed(f"env GIT_SSH_COMMAND={shlex.quote(ssh_command)} git -C /root/work push -q kajiba-git@127.0.0.1:alice/demo.git {sha}:refs/heads/main 2>&1")
          query = f"SELECT public_id FROM run WHERE sha = '{sha}' AND trigger = 'push'"
          forge.wait_until_succeeds(f"test -n \"$(sqlite3 {db} {shlex.quote(query)})\"")
          run_id = sql(query)
          # ADR 0132: the pusher sees the run the push created.
          assert f"kajiba: CI run {run_id} on refs/heads/main" in out, out
          return run_id


      def finished(run_id):
          query = f"SELECT state FROM run WHERE public_id = '{run_id}'"
          forge.wait_until_succeeds(f"sqlite3 {db} {shlex.quote(query)} | grep -Ex 'succeeded|failed|cancelled'")
          return sql(query)


      def attempts(run_id):
          # [(public id, number, state)] of the run's only job.
          out = sql(f"SELECT a.public_id, a.number, a.state FROM job_attempt a JOIN job ON job.id = a.job_id JOIN run ON run.id = job.run_id WHERE run.public_id = '{run_id}' ORDER BY a.number")
          return [tuple(line.split("|")) for line in out.splitlines()]


      def log_of(attempt_id):
          return forge.succeed(f"cat {logs_dir}/{attempt_id}.log")


      def started(run_id):
          # The current attempt's log shows the long check running.
          forge.wait_until_succeeds(f"grep -q long-start {logs_dir}/$(sqlite3 {db} \"SELECT a.public_id FROM job_attempt a JOIN job ON job.id = a.job_id JOIN run ON run.id = job.run_id WHERE run.public_id = '{run_id}' ORDER BY a.number DESC LIMIT 1\").log")


      def finishes(run_id):
          return int(sql(f"SELECT COUNT(*) FROM event WHERE kind = 'run-finished' AND payload LIKE '%{run_id}%'"))


      def watch_requests():
          return [line for line in unit_lines(forge, "kajiba-server.service") if line.get("event") == "request" and line.get("method") == "/kajiba.v1.RunService/WatchRun"]


      def has_reason(out, reason):
          # grpcurl prints an ErrorInfo it cannot resolve as base64 `@value`.
          values = re.findall(r'"@value": "([A-Za-z0-9+/=]+)"', out)
          return reason in out or any(reason.encode() in base64.b64decode(value) for value in values)


      def hello(hello_version, tags):
          # The runner's SVID through the relay. grpcurl would check a DNS name the SVID lacks,
          # hence -insecure: what is tested is the forge checking the client.
          message = {"hello": {"name": "build-1", "version": hello_version, "hostname": "grpcurl", "capabilities": {"arch": "${pkgs.stdenv.hostPlatform.parsed.cpu.name}", "os": "linux", "backends": ["nix"], "tags": tags, "maxJobs": 1}}}
          code, out = forge.execute(f"grpcurl -insecure -cert ${svid.ca}/runner/svid.pem -key ${svid.ca}/runner/key.pem -import-path {proto} -proto kajiba/runner/v1/runner.proto -d {shlex.quote(json.dumps(message))} runners.forge:443 kajiba.runner.v1.RunnerService/Connect 2>&1")
          assert code != 0, out
          return out


      def manual_runner(config, seconds, env=""):
          # A second runner process on the same host, as the runner user, for `seconds`.
          runner.succeed(f"printf %s {shlex.quote(config)} > {manual}/runner.toml")
          return runner.execute(f"runuser -u kajiba-runner -- env PATH=/run/current-system/sw/bin HOME={manual} {env} timeout {seconds} kajiba-runner --config {manual}/runner.toml 2>&1")


      def runner_toml(extra, files="/var/lib/hanko/kajiba-runner"):
          return f'name = "build-1"\nurl = "https://runners.forge"\nstate_dir = "{manual}"\n{extra}[tls]\ntrust_domain = "example.org"\nserver_id = "${svid.serverId}"\nsvid_file = "{files}/svid.pem"\nkey_file = "{files}/key.pem"\nbundle_file = "{files}/bundle.pem"\n'


      start_all()
      forge.wait_for_unit("kajiba-server.service")
      forge.wait_for_unit("kajiba-sshd.service")
      forge.wait_for_unit("nginx.service")
      runner.wait_for_unit("kajiba-runner.service")
      forge.succeed(
          "install -m 600 ${./fixtures/ssh-writer-key} /root/writer-key",
          "install -m 600 ${./fixtures/ssh-signing-key} /root/signing-key",
          "ssh-keyscan -p 2222 127.0.0.1 > /root/known_hosts",
          "umask 077; printf '%s\\n%s\\n' https://forge '${writerPat}' > /root/writer-token",
          "umask 077; printf '%s\\n%s\\n' https://forge '${readerPat}' > /root/reader-token",
          "git init -q -b main /root/work",
          "git -C /root/work config user.name Writer",
          "git -C /root/work config user.email writer@example.org",
          "git -C /root/work config gpg.format ssh",
          "git -C /root/work config user.signingkey /root/signing-key",
          # Incompressible: the first bundle is large, later ones only carry new commits.
          "head -c 600000 /dev/urandom > /root/work/blob",
          "mkdir -p -m 0755 /var/lib/kajiba-fixtures",
      )
      runner.succeed(f"install -d -o kajiba-runner -g kajiba-runner -m 0700 {manual}")
      repo_dir = "/var/lib/kajiba-git/repos/" + sql("SELECT public_id FROM repo WHERE config_key = 'demo'") + ".git"
    ''
    + script
    + ''

      validate_logs(forge, "kajiba-server.service")
      validate_logs(forge, "kajiba-git.service")
      validate_logs(runner, "kajiba-runner.service")
      validate_metrics(forge, metrics_url)
      validate_metrics(runner, "http://127.0.0.1:9432/metrics")
    '';
}
