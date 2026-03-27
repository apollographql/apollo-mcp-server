---
default: patch
---

# Fix aarch64 musl installer fallback

The install script previously blocked installation on `aarch64` Linux systems with older glibc versions, even though a musl binary is now available for that target. This change removes the block, allowing ARM64 musl systems to fall back to the musl binary just like `x86_64` systems.

The installer script is now testable via `source` without side effects, and a new `test-install.sh` validates architecture detection and download URL logic across OS, CPU, and libc combinations.
