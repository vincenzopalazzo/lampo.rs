{
  description = "Lampo Nix Flake Shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    naersk.url = "github:nix-community/naersk";
  };

  outputs = { self, nixpkgs, flake-utils, naersk }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        # Our integration tests required the cln and bitcoind
        # so in this variable we declare everthin that we need
        # for bitcoin and cln
        cln-env-shell = with pkgs; [ clightning bitcoind ];

        # build rust application :)
        naersk' = pkgs.callPackage naersk { };
      in
      rec {


        # Set up the nix flake derivation
        packages = {
          # build the deamon binary
          lampod = naersk'.buildPackage {
            # name of the binary
            name = "lampod-cli";
            version = "0.0.1";
            src = ./.;
            nativeBuildInputs = with pkgs; [ pkg-config ];
            buildInputs = with pkgs; [ bitcoind openssl ];

            # lampo is broken at the moment in release mode
            release = false;
          };

          # build the lampo-cli
          cli = naersk'.buildPackage {
            # name of the binary
            name = "lampo-cli";
            version = "0.0.1";
            src = ./.;
            nativeBuildInputs = with pkgs; [ pkg-config ];
            buildInputs = with pkgs; [ openssl ];

            # lampo is broken at the moment in release mode
            release = false;
          };

          default = packages.lampod;
        };
        # FIXME: will be good to have this formatting also the rust code
        formatter = pkgs.nixpkgs-fmt;

        devShell = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ gnumake rustup openssl ] ++ cln-env-shell;
        };
      }
    );
}
