{
  description = "MCP Support for Apollo Tooling";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/release-24.11";

    # Helper utility for keeping certain paths from garbage collection in CI
    cache-nix-action = {
      url = "github:nix-community/cache-nix-action";
      flake = false;
    };

    # Rust builder
    crane.url = "github:ipetkov/crane";

    # Rust overlay for toolchain / building deterministically
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Overlay for common architecture support
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    cache-nix-action,
    crane,
    nixpkgs,
    fenix,
    flake-utils,
  } @ inputs:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {inherit system;};
      toolchain = fenix.packages.${system}.fromToolchainFile {
        file = ./rust-toolchain.toml;
        sha256 = "sha256-X/4ZBHO3iW0fOenQ3foEvscgAPJYl2abspaBThDOukI=";
      };

      # Rust options
      systemDependencies =
        (with pkgs; [
          openssl
        ])
        ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.libiconv
        ];

      # Crane options
      craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
      craneCommonArgs = {
        inherit src;
        pname = "mcp-apollo";
        strictDeps = true;

        nativeBuildInputs = with pkgs; [pkg-config];
        buildInputs = systemDependencies;
      };
      # Build the cargo dependencies (of the entire workspace), so we can reuse
      # all of that work (e.g. via cachix) when running in CI
      cargoArtifacts = craneLib.buildDepsOnly craneCommonArgs;
      src = craneLib.cleanCargoSource ./.;

      # Supporting tools
      mcphost = pkgs.callPackage ./nix/mcphost.nix {};
      mcp-server-tools = pkgs.callPackage ./nix/mcp-server-tools {};

      # CI options
      garbageCollector = import "${inputs.cache-nix-action}/saveFromGC.nix" {
        inherit pkgs inputs;
        derivations = [cargoArtifacts toolchain] ++ mcp-server-tools;
      };
    in {
      devShells.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs; [pkg-config];
        buildInputs =
          [
            mcphost
            toolchain
          ]
          ++ mcp-server-tools
          ++ systemDependencies
          ++ (with pkgs; [
            # For running github action workflows locally
            act

            # For autogenerating nix evaluations for MCP server tools
            node2nix

            # Some of the mcp tooling likes to spawn arbitrary node runtimes,
            # so we need nodejs in the path here :(
            nodejs_22

            # For local LLM testing
            ollama

            # For consistent TOML formatting
            taplo
          ]);
      };

      checks = {
        clippy = craneLib.cargoClippy (craneCommonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });
        docs = craneLib.cargoDoc (craneCommonArgs
          // {
            inherit cargoArtifacts;
          });

        # Check formatting
        nix-fmt = pkgs.runCommandLocal "check-nix-fmt" {} "${pkgs.alejandra}/bin/alejandra --check ${./.}; touch $out";
        rustfmt = craneLib.cargoFmt {
          inherit src;
        };
        toml-fmt = craneLib.taploFmt {
          src = pkgs.lib.sources.sourceFilesBySuffices src [".toml"];
        };
      };

      packages = rec {
        default = mcp-apollo-server;
        mcp-apollo-server = let
          fileSetForCrate = crate:
            pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                (craneLib.fileset.commonCargoSources crate)
              ];
            };
        in
          craneLib.buildPackage (craneCommonArgs
            // {
              pname = "mcp-apollo-server";
              cargoExtraArgs = "-p mcp-apollo-server";
              src = fileSetForCrate ./crates/mcp-apollo-server;
            });

        # CI related packages
        inherit (garbageCollector) saveFromGC;
      };
    });
}
