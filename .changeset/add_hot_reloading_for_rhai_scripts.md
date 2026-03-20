---
default: minor
---

# Add hot reloading for Rhai scripts

Rhai scripts in the `rhai/` directory are now automatically reloaded when changes are detected. You no longer need to restart the server after editing your Rhai hook scripts. Modifications to `main.rhai` or any file in the `rhai/` directory, including subdirectories, are picked up automatically.

If a script fails to compile during a reload, the server logs the error and continues running with the previous version of your scripts.
