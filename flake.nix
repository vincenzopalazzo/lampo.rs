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

        clightning = pkgs.clightning.overrideAttrs (oldAttrs: {
          version = "master-62dd";
          src = pkgs.fetchgit {
            url = "https://github.com/ElementsProject/lightning";
            rev = "62ddf84b4f15a460ed5a8f72b313998a83eefe19";
            sha256 = "sha256-Xl7mrV9dSATfYIFvhK9qPyRQM2gZGPV27I/5ic5avpM=";
            fetchSubmodules = true;
          };
          configureFlags = [ "--disable-rust" "--disable-valgrind" ];
        } // pkgs.lib.optionalAttrs (!pkgs.stdenv.isDarwin) {
          NIX_CFLAGS_COMPILE = "-Wno-stringop-truncation -w";
        });
        # Our integration tests required the cln and bitcoind
        # so in this variable we declare everthin that we need
        # for bitcoin and cln
        cln-env-shell = [ clightning pkgs.bitcoind ];

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

          # FIXME: add a target to run integration testing
          # FIXME: add a target to run lnprototest

          default = packages.lampod;
        };
        # FIXME: will be good to have this formatting also the rust code
        formatter = pkgs.nixpkgs-fmt;

        devShell = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ gnumake rustc cargo rustfmt openssl openssl.dev ] ++ cln-env-shell;
          shellHook = ''
            export HOST_CC=gcc
            export RUST_BACKTRACE=1
          '';
        };
      }
    );
}
