# J0's only integration test: the module starts kajiba-server and /healthz answers (0015).
{ pkgs, kajibaServerPackage }:
pkgs.testers.nixosTest {
  name = "kajiba-healthz";

  nodes.forge = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      http.publicUrl = "https://example.org";
    };
  };

  testScript = ''
    forge.wait_for_unit("kajiba-server.service")
    forge.wait_for_open_port(8080)
    forge.succeed("curl -sf http://localhost:8080/healthz")
  '';
}
