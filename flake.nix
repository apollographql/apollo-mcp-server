{
  description = "MCP Support for Apollo Tooling";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/release-24.11";
    unstable.url = "github:nixos/nixpkgs/nixpkgs-unstable";

    # Helper utility for keeping certain paths from garbage collection in CI
    cache-nix-action = {
      url = "github:nix-community/cache-nix-action";
      flake = false;
    };

    # Rust builder
    crane.url = "github:ipetkov/crane";

    # Rust toolchains
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "unstable";
    };

    # Overlay for common architecture support
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    cache-nix-action,
    crane,
    flake-utils,
    nixpkgs,
    rust-overlay,
    unstable,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = nixpkgs.legacyPackages.${system};
      unstable-pkgs = import unstable {
        inherit system;
        overlays = [(import rust-overlay)];
      };

      # Define our toolchains for both native and cross compilation targets
      nativeToolchain = p: p.rust-bin.stable.latest.default;
      mcp-apollo-builder = unstable-pkgs.callPackage ./nix/mcp-apollo.nix {
        inherit crane;
        toolchain = nativeToolchain;
      };

      # Supporting tools
      mcphost = pkgs.callPackage ./nix/mcphost.nix {};
      mcp-server-tools = pkgs.callPackage ./nix/mcp-server-tools {};
    in rec {
      devShells.default = pkgs.mkShell {
        nativeBuildInputs = mcp-apollo-builder.nativeDependencies;
        buildInputs =
          [
            mcphost
            (nativeToolchain unstable-pkgs)
          ]
          ++ mcp-apollo-builder.dependencies
          ++ mcp-server-tools
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

      checks =
        {
          # Check formatting
          nix-fmt = pkgs.runCommandLocal "check-nix-fmt" {} "${pkgs.alejandra}/bin/alejandra --check ${./.}; touch $out";
        }
        // mcp-apollo-builder.checks;

      packages = rec {
        default = mcp-apollo;
        mcp-apollo = mcp-apollo-builder.packages.mcp-apollo;

        crossBinaries = let
          crossTargets = [
            "aarch64-apple-darwin"
            "aarch64-unknown-linux-gnu"
            "aarch64-unknown-linux-musl"
            "x86_64-apple-darwin"
            "x86_64-pc-windows-msvc"
            "x86_64-unknown-linux-gnu"
            "x86_64-unknown-linux-musl"
          ];
          crossToolchain = p:
            p.rust-bin.stable.latest.minimal.override {
              # Pull in the various supported targets for cross compilation
              targets = crossTargets;
            };
          mcp-apollo-cross = unstable-pkgs.callPackage ./nix/mcp-apollo.nix {
            inherit crane;
            toolchain = crossToolchain;
          };

          # Individual builds for each supported platform
          linux-aarch64-gnu = mcp-apollo-cross.packages.builder "aarch64-unknown-linux-gnu";
          linux-aarch64-musl = mcp-apollo-cross.packages.builder "aarch64-unknown-linux-musl";
          linux-x86_64-gnu = mcp-apollo-cross.packages.builder "x86_64-unknown-linux-gnu";
          linux-x86_64-musl = mcp-apollo-cross.packages.builder "x86_64-unknown-linux-musl";
          linux = pkgs.runCommandLocal "linux-bundle" {} ''
            mkdir -p $out/bin

            cp ${linux-aarch64-musl}/bin/mcp-apollo-server $out/bin/mcp-apollo-server.linux-aarch64-musl
            cp ${linux-aarch64-gnu}/bin/mcp-apollo-server $out/bin/mcp-apollo-server.linux-aarch64-gnu
            cp ${linux-x86_64-musl}/bin/mcp-apollo-server $out/bin/mcp-apollo-server.linux-x86_64-musl
            cp ${linux-x86_64-gnu}/bin/mcp-apollo-server $out/bin/mcp-apollo-server.linux-x86_64-gnu
          '';

          macos-aarch64 = mcp-apollo-cross.packages.builder "aarch64-apple-darwin";
          macos-x86_64 = mcp-apollo-cross.packages.builder "x86_64-apple-darwin";
          macos = pkgs.runCommandLocal "macos-bundle" {} ''
            mkdir -p $out/bin
            ${unstable-pkgs.lipo-go}/bin/lipo -create \
                -output $out/bin/mcp-apollo-server.macos \
                -arch arm64 ${macos-aarch64}/bin/mcp-apollo-server
                # TODO: macos-x86_64 seems to cause a bad relocation error in hyper
                # -arch x86_64 ''${macos-x86_64}/bin/mcp-apollo-server

            cp ${macos-aarch64}/bin/mcp-apollo-server $out/bin/mcp-apollo-server.macos-aarch64
            # TODO: macos-x86_64 seems to cause a bad relocation error in hyper
            # cp ''${macos-aarch64}/bin/mcp-apollo-server $out/bin/mcp-apollo-server.macos-x86_64
          '';
        in
          unstable-pkgs.symlinkJoin {
            name = "mcp-apollo-cross-binaries";
            paths = [
              linux
              macos
            ];
          };
      };

      # TODO: This does not work on macOS without cross compiling, so maybe
      # we need to disable flake-utils and manually specify the supported
      # hosts?
      apps = let
        # Nix flakes don't yet expose a nice formatted timestamp in ISO-8601
        # format, so we need to drop out to date to do so.
        commitDate = pkgs.lib.readFile "${pkgs.runCommand "git-timestamp" {env.when = self.lastModified;} "echo -n `date -d @$when --iso-8601=seconds` > $out"}";
        builder = pkgs.dockerTools.streamLayeredImage {
          name = "mcp-apollo";
          tag = "latest";

          # Use the latest commit time for reproducible builds
          created = commitDate;
          mtime = commitDate;

          contents = [
            packages.mcp-apollo-server
          ];

          config = {
            # Make the entrypoint the server
            Entrypoint = ["mcp-apollo-server" "-d" "/data"];

            # Drop to local user
            User = "1000";
            Group = "1000";
          };
        };
      in {
        streamImage = {
          type = "app";
          program = "${builder}";
          meta.description = "Builds the mcp-apollo-server container and streams the image to stdout.";
        };
      };
    });
}
