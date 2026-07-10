# Network Isolation Experiment Iteration Report

## Iteration 1

- Goal: create one sandbox and one `--network-profile isolated` workspace session.
- Failure: `sandbox-cli runtime --sandbox-id <id> create_workspace_session --network-profile isolated` returned `network setup failed: nft ... No such file or directory`.
- Root cause: the Docker backend starts the uploaded daemon in a stock `ubuntu:24.04` container, but isolated networking shells out to `nft` and the image does not include `nftables`.
- Fix: `crates/sandbox-provider-docker/src/launch.rs` now bootstraps `nftables` with `apt-get` when `nft` is missing, then `exec`s the daemon with the original arguments.
- Local check: `cargo test -p sandbox-provider-docker --test launch` passed.
- Rerun: restart/rebuild the gateway, then retry isolated workspace creation.

## Iteration 2

- Goal: rerun isolated workspace creation after the Docker launch bootstrap.
- Failure: the same isolated-session command still returned `nft ... No such file or directory`.
- Evidence: `docker inspect` showed the shell bootstrap was present; `docker logs` showed `apt-get update` could not resolve `ports.ubuntu.com`, then `apt-get install` could not locate `nftables`.
- Root cause: apt did not use the uppercase `HTTP_PROXY` / `HTTPS_PROXY` env injected into the container.
- Fix: the Docker launch bootstrap now maps uppercase proxy env to lowercase `http_proxy` / `https_proxy` / `no_proxy` before running apt, and chains `apt-get update && apt-get install`.
- Local check: `cargo test -p sandbox-provider-docker --test launch` passed.
- Rerun: restart/rebuild the gateway, then retry isolated workspace creation.

## Iteration 3

- Goal: rerun isolated workspace creation after proxy-aware bootstrap.
- Failure: `create_sandbox` timed out while apt was still downloading and installing packages during daemon startup.
- Root cause: installing `nftables` at sandbox startup is not a universal fix. It assumes a Debian/Ubuntu-style package manager, assumes network/proxy behavior inside the container, and can exceed the daemon readiness timeout.
- Fix: backed out the launch-time package installation. The first replacement plan was a prebuilt test image with `nftables`, but that was superseded by the no-install implementation in Iteration 4.
- Local check: rerun `cargo test -p sandbox-provider-docker --test launch`.
- Rerun: build the test image once, restart the gateway, create a sandbox from that image, then create the three isolated workspace sessions.

## Iteration 4

- Goal: make isolated workspace networking require no package installation and work across Docker image families.
- Failure: the previous image/bootstrap approaches still depended on distro package availability or a specially baked image.
- Root cause: isolated networking installed static nftables rules through the `nft` CLI even though the workspace-to-workspace isolation path already uses Linux netlink for veth, bridge, and bridge-port isolation.
- Fix: removed the `nft`/netfilter module from workspace runtime initialization. Isolated workspaces now create the bridge and veths through rtnetlink only, require bridge-port isolation to succeed, and fail closed for `rfc1918_egress=deny` because that mode still needs packet filtering.
- Local check: `cargo test -p sandbox-runtime-workspace` passed.
- Rerun: rebuild and restart the Docker gateway, then run the three-workspace port-3000 isolation experiment on the stock image.

## Iteration 5

- Goal: rebuild and restart the Docker gateway after the no-install network fix.
- Failure: `bin/start-sandbox-docker-gateway --rebuild-binary` packaged the configured ARM daemon successfully, then failed trying to also build `x86_64-unknown-linux-musl` because `x86_64-linux-musl-gcc` is not installed locally.
- Root cause: `--rebuild-binary` ignored `manager.docker.daemon_binary_path` and forced both ARM and x86_64 daemon packages, unlike the normal path which packages only the configured target.
- Fix: `--rebuild-binary` now packages only the target inferred from the configured daemon artifact path.
- Local check: `sh -n bin/start-sandbox-docker-gateway` passed.
- Rerun: `bin/start-sandbox-docker-gateway --rebuild-binary` passed and restarted the gateway with `dist/sandbox-daemon-linux-arm64`.

## Iteration 6

- Goal: run the three-workspace port-3000 isolation experiment on stock `ubuntu:24.04`.
- Failure: the new live test failed before sandbox creation because direct `rustc --target aarch64-unknown-linux-musl` used the macOS linker for the static helper binary.
- Root cause: the helper compile did not pass the repo's Linux-musl linker setting.
- Fix: the live test now compiles the helper with `-C linker=rust-lld`, matching the repo packaging path.
- Local check: `python3 -m py_compile cli-operation-e2e-live-test/runtime/network_isolation/test_network_isolation.py` passed.
- Rerun: `pytest -q cli-operation-e2e-live-test/runtime/network_isolation/test_network_isolation.py -s` passed. It created one sandbox, created three `--network-profile isolated` workspace sessions, started one server per session on port 3000, verified each session could reach its own `127.0.0.1:3000`, verified all six cross-workspace `10.244.0.x:3000` attempts failed, then destroyed all sessions and the sandbox.
