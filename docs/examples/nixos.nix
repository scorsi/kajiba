# Import kajiba.nixosModules.server and provide its server/git/ui packages first.
{
  services.kajiba-server = {
    enable = true;
    http.publicUrl = "https://forge.example.org";
    users.alice.isAdmin = true;
    repos.demo = {
      owner = "alice";
      description = "A demonstration repository";
    };
    dump = {
      dir = "/var/lib/kajiba-dumps";
      keep = 7;
    };
  };
}
