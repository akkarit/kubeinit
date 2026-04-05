# kubeinit

A single-binary CLI tool written in Rust that bootstraps vanilla Kubernetes clusters using **kubeadm** with **Cilium** as the default CNI.

kubeinit handles the full lifecycle: installing dependencies, initializing the control plane, deploying Cilium in kube-proxy replacement mode, joining worker nodes, and tearing everything down.

## Features

- **Zero-to-cluster in one command** — automatically downloads and installs all required binaries (containerd, runc, kubeadm, kubelet, kubectl, crictl, Helm, Cilium CLI)
- **Cilium CNI with kube-proxy replacement** — kube-proxy is skipped during `kubeadm init`; Cilium runs with `kubeProxyReplacement=true`
- **Auto-detection** — detects the host's default IP and hostname for the control-plane endpoint; confirms interactively or accepts a custom value
- **Privilege escalation** — runs as root or automatically elevates via `sudo` when needed
- **Preflight checks** — validates swap, kernel modules (`br_netfilter`, `overlay`), sysctl settings, and required binaries before proceeding
- **Clean uninstall** — removes all installed binaries, systemd units, configuration, and data directories

## Requirements

- Linux (amd64 or arm64)
- Rust 1.85+ (to build)
- A systemd-based host with network access to download components

## Building

```bash
cargo build --release
```

The binary is produced at `target/release/kubeinit`.

## Usage

### Initialize a cluster

```bash
# Auto-detect control-plane endpoint (interactive confirmation)
kubeinit init

# Specify everything explicitly
kubeinit init \
  --control-plane-endpoint 192.168.1.100 \
  --pod-network-cidr 10.244.0.0/16 \
  --service-cidr 10.96.0.0/12 \
  --kubernetes-version 1.35.3 \
  --cilium-version 0.19.2
```

The `init` command will:

1. Install all required dependencies (skipping any already present)
2. Detect and confirm the control-plane endpoint
3. Run preflight checks
4. Run `kubeadm init` (with kube-proxy skipped)
5. Copy the admin kubeconfig to `~/.kube/config`
6. Install Cilium CNI (via `cilium` CLI or Helm fallback)
7. Print a summary of installed binaries, credentials, and config paths

### Install dependencies only

```bash
kubeinit install-deps
kubeinit install-deps --kubernetes-version 1.35.3 --cilium-version 0.19.2
```

### Run preflight checks

```bash
kubeinit preflight
```

### Join worker nodes

Run on the control plane to get the join command:

```bash
kubeinit join-token
```

Then run the printed `kubeadm join ...` command on each worker node.

### Check cluster status

```bash
kubeinit status
```

### Reset the cluster

```bash
kubeinit reset --force
```

Runs `kubeadm reset`, removes CNI configuration, and flushes iptables rules.

### Uninstall all components

```bash
kubeinit uninstall         # interactive confirmation
kubeinit uninstall --force # skip confirmation
```

Removes all binaries, systemd units, and data directories installed by kubeinit:

Default versions are defined in [`versions.toml`](versions.toml):

| Component | Default Version |
|-----------|----------------|
| containerd | 2.2.2 |
| runc | 1.4.2 |
| CNI plugins | 1.9.1 |
| crictl | 1.35.0 |
| kubeadm, kubelet, kubectl | 1.35.3 |
| Helm | 4.1.3 |
| Cilium CLI | 0.19.2 |

## Project Structure

```
├── versions.toml  # Version matrix — single source of truth for all component versions
├── build.rs       # Generates Rust constants from versions.toml at compile time
└── src/
    ├── main.rs    # CLI definition (clap) and command dispatch
    ├── cmd/       # Shell command execution helpers (run, run_privileged, etc.)
    ├── cluster/   # Cluster lifecycle: preflight, init, reset, status, join-token
    ├── cni/       # Cilium installation (cilium CLI preferred, Helm fallback)
    ├── deps/      # Dependency download and installation/uninstallation
    └── net/       # Network detection (default IP, hostname)
```

## License

MIT
