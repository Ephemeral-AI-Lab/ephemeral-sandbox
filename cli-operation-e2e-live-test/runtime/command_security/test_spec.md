# Command Security Live Tests

This suite implements the live cases from
`docs/obsidian/ephemeral-os/implementation_plan/daemon-command-syscall-hardening-test-case.md`.

Run with:

```bash
cd cli-operation-e2e-live-test
E2E_REBUILD_BINARY=1 pytest runtime/command_security -v
```

The default suite validates enforce mode. The package install smoke rewrites the
disposable sandbox's Ubuntu source file to the main archive pockets, uses a
root-owned archive cache under `/tmp`, uses apt's native
`APT::Sandbox::User=root` option because the existing user namespace setup
disables `setgroups`, and polls the command session instead of holding one
daemon request open; it skips only if the current network profile has no package
egress.

`fchmodat2` is treated as allowed when it returns either `OK` or `ENOSYS` in
live probes, because older or emulated kernels may not implement the syscall.
`EPERM` still fails the case.

For config-mode passes, restart the gateway with a matching
`manager.command_security.mode` config and set `E2E_COMMAND_SECURITY_MODE` to
`enforce`, `relaxed`, or `off`. The Docker config used for that pass must also
point `manager.docker.daemon_config_yaml_path` at the same mode-specific YAML,
because that is the file uploaded into each sandbox daemon.

For cross-architecture Docker runs, set `E2E_COMMAND_SECURITY_TARGET` to the
Linux musl target that matches the sandbox container, for example
`x86_64-unknown-linux-musl`.
