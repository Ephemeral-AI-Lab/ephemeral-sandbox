# Shell Security Live Tests

This suite implements the live cases from
`docs/obsidian/ephemeral-os/implementation_plan/daemon-command-syscall-hardening-test-case.md`.

Run with:

```bash
cd cli-operation-e2e-live-test
E2E_REBUILD_BINARY=1 pytest runtime/shell_security -v
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
