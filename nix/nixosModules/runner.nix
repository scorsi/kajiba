# Stub module: kajiba-runner has no protocol to speak yet (J6, 0008). Exists now so the flake
# output shape is stable for sekkeizu.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.kajiba-runner;
in
{
  options.services.kajiba-runner = {
    enable = lib.mkEnableOption "the kajiba runner";
    package = lib.mkPackageOption pkgs "kajiba-runner" { };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.kajiba-runner = {
      description = "kajiba runner";
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package}";
        DynamicUser = true;
      };
    };
  };
}
