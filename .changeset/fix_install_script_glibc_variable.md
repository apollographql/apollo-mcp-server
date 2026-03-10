---
default: patch
---

# Fix unbound variable in install script glibc check

The `has_required_glibc` function in the nix install script referenced an undefined `_libc_version` variable in its error message. Because the script runs with `set -u`, this caused a crash instead of printing the intended diagnostic on systems with glibc < 2.38 (e.g. Red Hat UBI9). The variable is now correctly referenced as `_glibc_version`.
