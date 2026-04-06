# kubeinit

A single-binary CLI tool written in Rust that bootstraps vanilla Kubernetes clusters using **kubeadm** with **Cilium** CNI, **Gateway API**, and **OpenEBS LocalPV** storage out of the box.

kubeinit handles the full lifecycle: installing dependencies, initializing the control plane, deploying networking and storage, joining worker and control-plane nodes, and tearing everything down.

## Features

- **Zero-to-cluster in one command** — automatically downloads and installs all required binaries (containerd, runc, kubeadm, kubelet, kubectl, crictl, Helm, Cilium CLI)
- **Cilium CNI with kube-proxy replacement** — kube-proxy is skipped during `kubeadm init`; Cilium runs with `kubeProxyReplacement=true`
- **Gateway API** — installs Kubernetes Gateway API CRDs (experimental channel, server-side apply) and enables Cilium's Gateway API with host-network mode by default
- **OpenEBS LocalPV storage** — installs OpenEBS Dynamic LocalPV Provisioner via Helm by default (hostpath-based local storage with default StorageClass)
- **Single-node ready** — removes the control-plane NoSchedule taint so workloads can run on the initial node
- **Multi-node support** — join additional nodes as workers or control-plane members via `kubeinit join`
- **Auto-detection** — detects the host's default IP and hostname for the control-plane endpoint; confirms interactively or accepts a custom value
- **Privilege escalation** — runs as root or automatically elevates via `sudo` when needed
- **Preflight auto-fix** — loads kernel modules (`br_netfilter`, `overlay`), sets sysctl parameters, and persists both across reboots
- **Clean uninstall** — gracefully drains the node, stops all containers, then removes binaries, systemd units, configuration, and data directories

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
  --cilium-version 0.19.2 \
  --storage-version 4.4.0

# Skip optional components
kubeinit init --skip-cni --skip-storage --gateway-api=false
```

The `init` command will:

1. Install all required dependencies (skipping any already present)
2. Detect and confirm the control-plane endpoint
3. Run preflight checks (auto-fixes kernel modules and sysctl)
4. Run `kubeadm init` (with kube-proxy skipped)
5. Remove the control-plane NoSchedule taint (single-node ready)
6. Copy the admin kubeconfig to `~/.kube/config` (owned by the invoking user, not root)
7. Install Gateway API CRDs (server-side apply)
8. Install Cilium CNI with Gateway API host-network mode
9. Install OpenEBS Dynamic LocalPV Provisioner storage
10. Print a summary of installed binaries, credentials, and config paths

### Join nodes to the cluster

On the **control plane**, generate a join token:

```bash
# Worker join token
kubeinit join-token

# Control-plane join token (includes certificate key)
kubeinit join-token --control-plane
```

On the **joining node**, run:

```bash
# Join as a worker
kubeinit join --role worker \
  --api-server 192.168.1.100:6443 \
  --token abcdef.0123456789abcdef \
  --ca-cert-hash sha256:...

# Join as a control-plane node
kubeinit join --role control-plane \
  --api-server 192.168.1.100:6443 \
  --token abcdef.0123456789abcdef \
  --ca-cert-hash sha256:... \
  --certificate-key abc123...
```

The `join` command automatically installs dependencies and runs preflight checks before joining.

### Install dependencies only

```bash
kubeinit install-deps
kubeinit install-deps --kubernetes-version 1.35.3 --cilium-version 0.19.2
```

### Run preflight checks

```bash
kubeinit preflight
```

### Check cluster status

```bash
kubeinit status
```

### Reset the cluster

```bash
kubeinit reset --force
```

Uninstalls Helm releases (OpenEBS, Cilium), deletes Gateway API CRDs, drains the node, runs `kubeadm reset`, cleans up iptables/ipvs, and removes data directories and kubeinit-managed configs.

### Uninstall all components

```bash
kubeinit uninstall         # interactive confirmation
kubeinit uninstall --force # skip confirmation
```

Gracefully drains the node, stops all containers via crictl, runs `kubeadm reset`, then removes all binaries, systemd units, and data directories.

## Default Versions

All versions are defined in [`versions.toml`](versions.toml). Paths, URLs, and network defaults are in [`config.toml`](config.toml). Both are compiled into the binary at build time via `build.rs`.

| Component | Default Version |
|-----------|----------------|
| Kubernetes (kubeadm, kubelet, kubectl) | 1.35.3 |
| containerd | 2.2.2 |
| runc | 1.4.2 |
| CNI plugins | 1.9.1 |
| crictl | 1.35.0 |
| Helm | 4.1.3 |
| Cilium CLI | 0.19.2 |
| Gateway API CRDs | 1.5.1 |
| OpenEBS LocalPV | 4.4.0 |

## Project Structure

```
├── versions.toml  # Version matrix for all components
├── config.toml    # Paths, URLs, and network defaults
├── build.rs       # Generates Rust constants from versions.toml + config.toml
└── src/
    ├── main.rs    # CLI definition (clap) and command dispatch
    ├── config.rs  # Re-exports generated constants
    ├── cmd/       # Shell command execution (run, run_privileged, real_user, etc.)
    ├── cluster/   # Cluster lifecycle: preflight, init, join, reset, status, join-token
    ├── cni/       # Cilium CNI + Gateway API CRD installation
    ├── storage/   # OpenEBS LocalPV Provisioner installation
    ├── deps/      # Dependency download, installation, and uninstallation
    └── net/       # Network detection (default IP, hostname)
```

## License

MIT
