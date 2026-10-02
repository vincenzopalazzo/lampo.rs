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

        # Our integration tests require bitcoind
        # so in this variable we declare everything that we need
        # for bitcoin regtest
        bitcoin-env-shell = [ pkgs.bitcoind ];

        # build rust application :)
        naersk' = pkgs.callPackage naersk { };
      in
      rec {
        # Set up the nix flake derivation
        packages = {
          # build the daemon binary
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

          # FIXME: add a target to run integration testing

          default = packages.lampod;
        };
        # FIXME: will be good to have this formatting also the rust code
        formatter = pkgs.nixpkgs-fmt;

        devShell = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ gnumake rustup openssl openssl.dev ] ++ bitcoin-env-shell;
          shellHook = ''
            export HOST_CC=gcc
            export RUST_BACKTRACE=1
          '';
        };
      }
    );
}
