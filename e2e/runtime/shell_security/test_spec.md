# Shell Security Live Tests

This suite implements the live cases from `test_cases.md` (SS-E01…E10, SS-M01…M15,
SS-H01…H15 — 40 cases), split by tier into `test_shell_security_easy.py`,
`test_shell_security_medium.py`, and `test_shell_security_hard.py` and marked
`@pytest.mark.{easy,medium,hard}`. The former CS-01…CS-06 cases are folded into the
matching SS cases. Sandboxes are driven **only** through `sandbox-manager-cli`
(lifecycle, via `manager.management.helpers`) and `sandbox-runtime-cli` (commands
and files, via `core.cli.runtime`).

## Required image

The suite runs on **`ubuntu:24.04`**, pinned via the suite default
(`core.config.IMAGE`) or the `E2E_IMAGE` env var. Every `apt`/`util-linux` step
(SS-M06…M09, SS-H11 `setpriv`, SS-H12 `setcap`) assumes ubuntu24: the noble archive
pockets and the util-linux / libcap2-bin tooling shipped with that image.

Run with:

```bash
cd e2e
E2E_REBUILD_BINARY=1 python3 -m pytest runtime/shell_security -v
# a single tier
python3 -m pytest runtime/shell_security -m easy -v
```

The child spawned by `shell_exec` is always hardened in `enforce` mode:
`no_new_privs`, a targeted capability drop, and the seccomp-lite deny table.
There is no operator-facing mode knob — `shell_exec` applies `enforce`
unconditionally, so the suite validates enforce behavior only.

The package install smoke rewrites the disposable sandbox's Ubuntu source file to
the main archive pockets, uses a root-owned archive cache under `/tmp`, uses apt's
native `APT::Sandbox::User=root` option because the existing user namespace setup
disables `setgroups`, and polls the command session instead of holding one daemon
request open; it skips only if the current network profile has no package egress.

`fchmodat2` is treated as allowed when it returns either `OK` or `ENOSYS` in live
probes, because older or emulated kernels may not implement the syscall. `EPERM`
still fails the case.

For cross-architecture Docker runs, set `E2E_SHELL_SECURITY_TARGET` to the Linux
musl target that matches the sandbox container, for example
`x86_64-unknown-linux-musl`.
