{
  cargo-zigbuild,
  crane,
  fetchzip,
  lib,
  perl,
  pkg-config,
  pkgs,
  toolchain,
  stdenv,
  zig,
}: let
  graphqlFilter = path: _type: builtins.match ".*graphql$" path != null;
  srcFilter = path: type:
    (graphqlFilter path type) || (craneLib.filterCargoSources path type);

  # Crane options
  src = pkgs.lib.cleanSourceWith {
    src = ../.;
    filter = srcFilter;
    name = "source"; # Be reproducible, regardless of the directory name
  };

  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
  craneCommonArgs = {
    inherit src;
    pname = "mcp-apollo";
    strictDeps = true;

    nativeBuildInputs = [perl pkg-config];
    buildInputs = [];
  };

  # Generate a derivation for just the dependencies of the project so that they
  # can be cached across all of the various checks and builders.
  cargoArtifacts = craneLib.buildDepsOnly craneCommonArgs;
in {
  # Expose the list of build dependencies for inheriting in dev shells
  nativeDependencies = craneCommonArgs.nativeBuildInputs;
  dependencies = craneCommonArgs.buildInputs;

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
    mcp-apollo = craneLib.buildPackage craneCommonArgs;

    # Builder for mcp-apollo-server. Takes the rust target triple for specifying
    # the cross-compile target. Set the target to the same as the host for native builds.
    builder = target: let
      # Use the same apple-sdk as cargo-zigbuild
      apple-sdk = stdenv.mkDerivation {
        pname = "apple-sdk";
        version = "11.3";

        src = fetchzip {
          url = "https://github.com/phracker/MacOSX-SDKs/releases/download/11.3/MacOSX11.3.sdk.tar.xz";
          hash = "sha256-BoFWhRSHaD0j3dzDOFtGJ6DiRrdzMJhkjxztxCluFKo=";
        };

        phases = ["installPhase"];
        installPhase = ''
          mkdir -p $out
          cp -r "$src"/* "$out"
        '';

        passthru.sdkroot = "${apple-sdk}";
      };

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
        builtins.concatStringsSep " " [
          "SDKROOT=${apple-sdk.sdkroot}"
          "CARGO_ZIGBUILD_CACHE_DIR=$TMP/.cache/cargo-zigbuild"
          "ZIG_LOCAL_CACHE_DIR=$TMP/.cache/zig-local"
          "ZIG_GLOBAL_CACHE_DIR=$TMP/.cache/zig-global"

          "${cargo-zigbuild-patched}/bin/cargo-zigbuild ${cmd}"
        ];
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
          cargoExtraArgs = "--release --target ${zig-target}";
          cargoCheckCommand = mkCmd "check";
          cargoBuildCommand = mkCmd "zigbuild";

          # Make sure to compile it for the specified target
          CARGO_BUILD_TARGET = target;
        });
  };
}
