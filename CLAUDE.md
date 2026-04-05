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

kubeinit is a CLI tool that orchestrates `kubeadm` to bootstrap vanilla Kubernetes clusters with Cilium as the CNI.

**Module layout:**

- `src/main.rs` — CLI definition (clap derive) and top-level command dispatch. All subcommands are defined here as the `Commands` enum.
- `src/cmd/` — Low-level shell command execution helpers (`run`, `run_privileged`, `run_output`, `binary_exists`). All external process invocation goes through this module. Privileged operations auto-elevate via `sudo` when not root.
- `src/cluster/` — Kubernetes cluster lifecycle: preflight checks, `kubeadm init`, kubeconfig setup, join-token generation, status, and reset.
- `src/cni/` — Cilium CNI installation. Prefers the `cilium` CLI, falls back to Helm.
- `src/deps/` — Dependency installation and uninstallation. Default versions are compiled in from `versions.toml` via `build.rs`.
- `src/net/` — Network auto-detection (default IP from routing table, hostname).

**Key design decisions:**

- kube-proxy is **skipped** during `kubeadm init` (`--skip-phases addon/kube-proxy`) because Cilium runs with `kubeProxyReplacement=true`.
- All external commands are executed via async helpers in `cmd/` — never call `std::process::Command` or `tokio::process::Command` directly from other modules.
- The project is fully async (tokio runtime) even though current operations are sequential, to support future concurrent operations.
- Component versions are defined in `versions.toml` (single source of truth) and compiled into the binary via `build.rs`. To update defaults, edit `versions.toml` and rebuild.
