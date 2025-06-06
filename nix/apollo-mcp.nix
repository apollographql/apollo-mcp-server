{
  apple-sdk,
  cargo-zigbuild,
  crane,
  fetchzip,
  lib,
  perl,
  pkg-config,
  pkgs,
  rename,
  stdenv,
  toolchain,
  zig,
}: let
  graphqlFilter = path: _type: builtins.match ".*graphql$" path != null;
  testFilter = path: _type: builtins.match ".*snap$" path != null;
  srcFilter = path: type:
    (graphqlFilter path type) || (testFilter path type) || (craneLib.filterCargoSources path type);

  # Crane options
  src = pkgs.lib.cleanSourceWith {
    src = ../.;
    filter = srcFilter;
    name = "source"; # Be reproducible, regardless of the directory name
  };

  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
  craneCommonArgs = {
    inherit src;
    pname = "apollo-mcp";
    strictDeps = true;

    nativeBuildInputs = [perl pkg-config];
    buildInputs = [];

    # Meta information about the packages
    meta = {
      description = "Apollo MCP Server";
      homepage = "https://www.apollographql.com/docs/apollo-mcp-server";
      license = lib.licenses.mit;

      # The main binary that should be run when using `nix run`
      mainProgram = "apollo-mcp-server";
    };
  };

  # Generate a derivation for just the dependencies of the project so that they
  # can be cached across all of the various checks and builders.
  cargoArtifacts = craneLib.buildDepsOnly craneCommonArgs;
in {
  # Expose the list of build dependencies for inheriting in dev shells
  nativeDependencies = craneCommonArgs.nativeBuildInputs;
  dependencies = craneCommonArgs.buildInputs;

  # Expose derivations that should be cached in CI
  cache = [
    cargoArtifacts
  ];

  # Expose checks for the project used by the root nix flake
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

    rustfmt = craneLib.cargoFmt {
      inherit src;
    };
    toml-fmt = craneLib.taploFmt {
      src = pkgs.lib.sources.sourceFilesBySuffices src [".toml"];
    };
  };

  # List of packages exposed by this project
  packages = {
    apollo-mcp = craneLib.buildPackage craneCommonArgs;

    # Builder for apollo-mcp-server. Takes the rust target triple for specifying
    # the cross-compile target. Set the target to the same as the host for native builds.
    builder = target: let
      # Patch cargo-zigbuild until they fix missing MacOS arguments in the linker
      cargo-zigbuild-patched = cargo-zigbuild.overrideAttrs {
        patches = [./cargo-zigbuild.patch];
      };

      # Make sure to use glibc version 2.17 if building for gnu dynamically linked targets
      zig-target =
        if lib.strings.hasSuffix "gnu" target
        then "${target}.2.17"
        else target;

      # Helper for generating a command using cargo-zigbuild and other shell-expanded
      # env vars.
      mkCmd = cmd:
        builtins.concatStringsSep " " ((lib.optionals stdenv.isDarwin ["SDKROOT=${apple-sdk.sdkroot}"])
          ++ [
            "CARGO_ZIGBUILD_CACHE_DIR=$TMP/.cache/cargo-zigbuild"
            "ZIG_LOCAL_CACHE_DIR=$TMP/.cache/zig-local"
            "ZIG_GLOBAL_CACHE_DIR=$TMP/.cache/zig-global"

            "${cargo-zigbuild-patched}/bin/cargo-zigbuild ${cmd}"
          ]);
    in
      craneLib.buildPackage (craneCommonArgs
        // {
          pname = craneCommonArgs.pname + "-${target}";
          nativeBuildInputs = [
            cargo-zigbuild-patched
            pkg-config
            perl
            zig
          ];

          # It doesn't make sense to run checks on these since they are for a
          # different OS / arch.
          doCheck = false;

          # Use zig for both CC and linker since it actually supports cross-compilation
          # nicely.
          cargoExtraArgs = lib.strings.concatStringsSep " " ([
              "--target ${zig-target}"
            ]
            # x86_64-apple-darwin compilation has a bug that causes release builds to
            # fail with "bad relocation", so we build debug targets for it instead.
            # See: https://github.com/rust-cross/cargo-zigbuild/issues/338
            ++ (lib.optionals (target != "x86_64-apple-darwin") ["--release"]));

          cargoCheckCommand = mkCmd "check";
          cargoBuildCommand = mkCmd "zigbuild";

          # Make sure to compile it for the specified target
          CARGO_BUILD_TARGET = target;
        });
  };
}
