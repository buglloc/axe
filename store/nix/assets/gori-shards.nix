# Production dependencies from gori 0.9.1's nix/shards.nix.
{
  cvss = {
    url = "https://github.com/hahwul/cvss.cr.git";
    rev = "v0.3.0";
    sha256 = "016aky70cspqavviq38haxjkfy4p4yxa68m8k3070ml42p57s561";
  };
  db = {
    url = "https://github.com/crystal-lang/crystal-db.git";
    rev = "ef748a234dbee14ace4c72ad901d332981f4d57d";
    sha256 = "sha256-Nq/No5quVmxfIxLGu8Ak6698pL2xOhrLzLYhrdffubg=";
  };
  sarif = {
    url = "https://github.com/hahwul/sarif.cr.git";
    rev = "v0.2.0";
    sha256 = "0rd3yw20djr4sc3lnimj9hiv9aqn6vxf3cz48wpkgr7dmwqlfn6g";
  };
  sqlite3 = {
    url = "https://github.com/crystal-lang/crystal-sqlite3.git";
    rev = "91f6c13d8f1df08f2659dfce7261ec9c40666302";
    sha256 = "sha256-sIM1oAaVHxyW4OjBDLa4Y3bJY1Xx/UBd+WWv+XXr6k8=";
  };
  termisu = {
    url = "https://github.com/hahwul/termisu.git";
    rev = "ca964e5f9256990d9d1e6aa5ca3aa1fb2635fb6b";
    sha256 = "sha256-UnzoYfw8W67whoCX2pnFO53+qXBxpISkuS97HcsjoT8=";
  };
}
