# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo build                # dev build
cargo build --release      # release build
cargo run -- <subcommand>  # run directly (e.g. cargo run -- preflight)
cargo clippy               # lint
cargo fmt                  # format
cargo test                 # run all tests
cargo test <test_name>     # run a single test
```

The binary requires root privileges and a real Kubernetes-capable host to run most commands (kubeadm, kubelet, etc. must be installed).

## Architecture

kubeinit is a CLI tool that orchestrates `kubeadm` to bootstrap vanilla Kubernetes clusters with Cilium CNI, Gateway API, and Longhorn storage.

**Init order:** deps → preflight → kubeadm init → remove NoSchedule taint → kubeconfig → Gateway API CRDs → Cilium CNI (with Gateway API host-network) → open-iscsi + Longhorn storage → summary.

**Module layout:**

- `src/main.rs` — CLI definition (clap derive) and top-level command dispatch. All subcommands are defined here as the `Commands` enum.
- `src/config.rs` — Re-exports all build-time constants generated from `versions.toml` and `config.toml`. All modules import paths, URLs, and versions from here.
- `src/cmd/` — Low-level shell command execution helpers (`run`, `run_privileged`, `run_output`, `binary_exists`, `real_user`). All external process invocation goes through this module. Privileged operations auto-elevate via `sudo` when not root. `real_user()` resolves the invoking user behind sudo for correct file ownership.
- `src/cluster/` — Kubernetes cluster lifecycle: preflight checks (auto-loads kernel modules, sets sysctl), `kubeadm init`, `kubeadm join` (worker or control-plane), kubeconfig setup with proper ownership, join-token generation, status, and reset.
- `src/cni/` — Cilium CNI installation (prefers `cilium` CLI, falls back to Helm). Also handles Gateway API CRD installation (server-side apply) and enabling `gatewayAPI.enabled` + `gatewayAPI.hostNetwork.enabled` in Cilium.
- `src/storage/` — Longhorn distributed block storage installation via Helm. Automatically installs the open-iscsi host dependency (detects apt/dnf/yum/zypper/pacman).
- `src/deps/` — Dependency installation and uninstallation. Uninstall gracefully drains the node and stops all containers before removing binaries.
- `src/net/` — Network auto-detection (default IP from routing table, hostname).

**Key design decisions:**

- kube-proxy is **skipped** during `kubeadm init` (`--skip-phases addon/kube-proxy`) because Cilium runs with `kubeProxyReplacement=true`.
- Gateway API CRDs use `kubectl apply --server-side=true` because the CRDs are too large for client-side apply. Gateway API runs in host-network mode so envoy proxies bind directly to node IPs.
- The control-plane NoSchedule taint is removed after init so single-node clusters can schedule workloads (Longhorn, user pods).
- All external commands are executed via async helpers in `cmd/` — never call `std::process::Command` or `tokio::process::Command` directly from other modules.
- The project is fully async (tokio runtime) even though current operations are sequential, to support future concurrent operations.
- Component versions are defined in `versions.toml`, paths/URLs/network defaults in `config.toml`. Both are compiled into the binary via `build.rs`. To update defaults, edit those files and rebuild.
- No hardcoded versions, URLs, or filesystem paths exist in Rust source — all come from `config::*` constants.
