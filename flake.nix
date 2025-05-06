{
  description = "MCP Support for Apollo Tooling";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/release-24.11";
    unstable-pkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";

    # Rust builder
    crane.url = "github:ipetkov/crane";

    # Overlay for common architecture support
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    crane,
    nixpkgs,
    flake-utils,
    unstable-pkgs,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = nixpkgs.legacyPackages.${system};
      unstable = unstable-pkgs.legacyPackages.${system};

      # Rust options
      systemDependencies =
        (with pkgs; [
          openssl
        ])
        ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
          # pkgs.libiconv
        ];

      # Crane options
      craneLib = crane.mkLib unstable;
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
      src = let
        graphqlFilter = path: _type: builtins.match ".*graphql$" path != null;
        srcFilter = path: type:
          (graphqlFilter path type) || (craneLib.filterCargoSources path type);
      in
        pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = srcFilter;
          name = "source"; # Be reproducible, regardless of the directory name
        };

      # Supporting tools
      mcphost = pkgs.callPackage ./nix/mcphost.nix {};
      mcp-server-tools = pkgs.callPackage ./nix/mcp-server-tools {};
    in {
      devShells.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs; [pkg-config];
        buildInputs =
          [
            mcphost
          ]
          ++ mcp-server-tools
          ++ systemDependencies
          ++ (with unstable; [
            cargo
            rust-analyzer
            rustc
            rustfmt
          ])
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
        mcp-apollo-server = craneLib.buildPackage (craneCommonArgs
          // {
            pname = "mcp-apollo-server";
            cargoExtraArgs = "-p mcp-apollo-server";
          });
      };
    });
}
