# Stub module: same as nixosModules.runner, for a runner hosted on jiban (darwin, J13).
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
    launchd.daemons.kajiba-runner = {
      serviceConfig = {
        ProgramArguments = [ (lib.getExe cfg.package) ];
        RunAtLoad = true;
        KeepAlive = false;
      };
    };
  };
}
