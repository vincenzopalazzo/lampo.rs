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
          version = "master-3c48";
          src = pkgs.fetchgit {
            url = "https://github.com/ElementsProject/lightning";
            rev = "3c484388212928f3019a3f2bb820bd75520e3581";
            sha256 = "sha256-QIjk1Z0Vt5fx9T2CtokehZkr8Iv/orFK3h0OglzcxDA=";
            fetchSubmodules = true;
          };
          configureFlags = [ "--disable-rust" "--disable-valgrind" ];
          # FIXME: this changes need to be reported upstream
          buildInputs = [ pkgs.gmp pkgs.libsodium pkgs.sqlite pkgs.zlib pkgs.jq ];
          # this causes some python trouble on a darwin host so we skip this step.
          # also we have to tell libwally-core to use sed instead of gsed.
          postPatch = if !pkgs.stdenv.isDarwin then ''
              patchShebangs \
                 tools/generate-wire.py \
                 tools/fromschema.py \
                 tools/update-mocks.sh \
                 tools/mockup.sh \
                 devtools/sql-rewrite.py \
                 plugins/clnrest/clnrest.py
              '' else ''
                substituteInPlace external/libwally-core/tools/autogen.sh --replace gsed sed && \
                substituteInPlace external/libwally-core/configure.ac --replace gsed sed
              '';
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
