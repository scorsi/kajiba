# J1 task 30: the unit's journal passes nisshi with start, ready and request lines (request_id on
# every request), the loopback metrics endpoint passes masu and counts a request from another
# node, and that endpoint is unreachable from the other node even with its port open (0015, 0022).
# J3 task 15: the git-side endpoint counts completed import sessions with the same checks (0054).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
pkgs.testers.runNixOSTest {
  name = "kajiba-server-observability";

  nodes = {
    server = {
      imports = [ ../nixosModules/server.nix ];
      services.kajiba-git = {
        package = kajibaGitPackage;
        metrics.listen = "127.0.0.1:9433";
      };
      services.kajiba-server = {
        enable = true;
        package = kajibaServerPackage;
        http = {
          listen = "0.0.0.0:8080";
          publicUrl = "https://forge.example.org";
        };
        metrics.listen = "127.0.0.1:9431";
        users.alice = { };
        repos.demo = {
          owner = "alice";
          description = "Metrics import fixture";
        };
      };
      # Metrics ports are open too: only the loopback binds can keep the other node out.
      networking.firewall.allowedTCPPorts = [
        8080
        9431
        9433
      ];
      environment.systemPackages = [
        pkgs.curl
        pkgs.git
      ];
    };
    client.environment.systemPackages = [ pkgs.curl ];
  };

  testScript = testLib + ''
    import json
    import re
    import time

    unit = "kajiba-server.service"
    metrics_url = "http://127.0.0.1:9431/metrics"
    git_metrics_url = "http://127.0.0.1:9433/metrics"
    series = 'kajiba_requests_total{kind="other",code="2xx"}'


    def sample(url, series):
        text = server.succeed(f"curl -sf {url}")
        match = re.search(rf"^{re.escape(series)} (\d+)$", text, re.MULTILINE)
        assert match, f"{series} missing from {url}: {text}"
        return int(match.group(1))


    start_all()

    with subtest("the server is ready through the module"):
        server.wait_for_unit(unit)
        state = server.succeed(f"systemctl show -P ActiveState {unit}").strip()
        assert state == "active", f"ActiveState: {state!r}"

    with subtest("a request from the other node moves kajiba_requests_total"):
        before = sample(metrics_url, series)
        client.wait_for_unit("multi-user.target")
        code = client.succeed("curl -s -o /dev/null -w '%{http_code}' http://server:8080/healthz")
        assert code == "200", f"client /healthz: {code}"
        # The counter is updated once the response body is done: allow it a moment.
        after = before
        for _ in range(20):
            after = sample(metrics_url, series)
            if after > before:
                break
            time.sleep(0.5)
        assert after > before, f"{series} did not move: {before} -> {after}"

    with subtest("a completed import moves the git-side session counter"):
        completed = 'kajiba_git_sessions_total{kind="import",result="ok"}'
        before = sample(git_metrics_url, completed)
        bundle = "/var/lib/kajiba-fixtures/demo.bundle"
        make_demo(server, bundle)
        server.succeed(f"kajiba-server admin import alice/demo {bundle}")
        assert sample(git_metrics_url, completed) == before + 1
        assert sample(git_metrics_url, 'kajiba_git_sessions_active{kind="import"}') == 0
        assert sample(git_metrics_url, 'kajiba_git_sessions_total{kind="import",result="error"}') == 0

    with subtest("both loopback metrics endpoints pass masu"):
        validate_metrics(server, metrics_url)
        validate_metrics(server, git_metrics_url)

    with subtest("the metrics endpoint is not reachable from the other node"):
        for port in [9431, 9433]:
            status, out = client.execute(f"curl -s -o /dev/null --max-time 5 http://server:{port}/metrics")
            # curl exit 7: connection refused, i.e. nothing listens on the external address.
            assert status == 7, f"client curl to server:{port}: exit {status}, output {out!r}"

    with subtest("the journal passes nisshi with start, ready and request lines"):
        validate_logs(server, unit)
        validate_logs(server, "kajiba-git.service")
        git_out = server.succeed("journalctl _SYSTEMD_UNIT=kajiba-git.service -o cat --no-pager")
        git_lines = [json.loads(line) for line in git_out.splitlines()]
        listening = [line for line in git_lines if line["event"] == "metrics-listening"]
        assert len(listening) == 1 and listening[0]["addr"] == "127.0.0.1:9433", listening
        assert {"start", "ready", "session-started", "session-exited"} <= {line["event"] for line in git_lines}
        out = server.succeed(f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager")
        lines = [json.loads(line) for line in out.splitlines()]
        events = {line["event"] for line in lines}
        missing = {"start", "ready", "request"} - events
        assert not missing, f"missing {missing} in events {events}: {out}"
        requests = [line for line in lines if line["event"] == "request"]
        without = [line for line in requests if not line.get("request_id")]
        assert not without, f"request lines without request_id: {without}"
        print(f"{len(requests)} request lines, all with request_id: {[(r.get('method'), r['request_id']) for r in requests]}")
  '';
}
