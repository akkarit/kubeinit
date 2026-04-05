use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use serde::Serialize;
use tracing::info;

use crate::cmd;
use crate::config;

/// Configuration for a new cluster.
#[derive(Debug, Serialize)]
pub struct ClusterConfig {
    pub control_plane_endpoint: String,
    pub pod_network_cidr: String,
    pub service_cidr: String,
    pub kubernetes_version: Option<String>,
}

/// Role when joining an existing cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRole {
    /// Worker-only node
    Worker,
    /// Control-plane node (also runs workloads unless tainted)
    ControlPlane,
}

impl FromStr for NodeRole {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "worker" => Ok(Self::Worker),
            "control-plane" | "controlplane" | "master" | "both" => Ok(Self::ControlPlane),
            _ => bail!("Invalid role: {s}. Use 'worker', 'control-plane', or 'both'."),
        }
    }
}

impl fmt::Display for NodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Worker => write!(f, "worker"),
            Self::ControlPlane => write!(f, "control-plane"),
        }
    }
}

/// Configuration for joining an existing cluster.
#[derive(Debug)]
pub struct JoinConfig {
    pub role: NodeRole,
    pub token: String,
    pub api_server: String,
    pub ca_cert_hash: String,
    pub certificate_key: Option<String>,
}

/// Run preflight checks to ensure the host is ready.
pub async fn preflight_checks() -> Result<()> {
    info!("Running preflight checks...");

    // Required binaries
    let required = ["kubeadm", "kubelet", "kubectl", "crictl"];
    for bin in &required {
        if !cmd::binary_exists(bin).await {
            bail!("Required binary not found: {bin}. Install it before proceeding.");
        }
    }
    info!("All required binaries found");

    // Check that swap is disabled (kubeadm requirement)
    let swap_info = cmd::run_output("cat", &["/proc/swaps"]).await.unwrap_or_default();
    let swap_lines: Vec<&str> = swap_info.lines().skip(1).filter(|l| !l.is_empty()).collect();
    if !swap_lines.is_empty() {
        bail!("Swap is enabled. Disable swap before initializing the cluster: swapoff -a");
    }
    info!("Swap is disabled");

    // Load required kernel modules, persisting across reboots
    let modules_loaded = cmd::run_output("lsmod", &[]).await.unwrap_or_default();
    for module in ["br_netfilter", "overlay"] {
        if !modules_loaded.contains(module) {
            info!("Loading kernel module {module}...");
            cmd::run_privileged("modprobe", &[module]).await?;
        }
    }
    // Persist so they survive reboot
    cmd::run_privileged("bash", &[
        "-c",
        "printf 'overlay\nbr_netfilter\n' > /etc/modules-load.d/kubeinit.conf",
    ]).await?;
    info!("Required kernel modules loaded");

    // Set required sysctl parameters, persisting across reboots
    let sysctl_params = [
        "net.bridge.bridge-nf-call-iptables",
        "net.ipv4.ip_forward",
    ];
    for param in &sysctl_params {
        let val = cmd::run_output("sysctl", &["-n", param])
            .await
            .unwrap_or_default();
        if val.trim() != "1" {
            info!("Setting {param} = 1...");
            cmd::run_privileged("sysctl", &["-w", &format!("{param}=1")]).await?;
        }
    }
    // Persist so they survive reboot
    cmd::run_privileged("bash", &[
        "-c",
        "printf 'net.bridge.bridge-nf-call-iptables=1\nnet.ipv4.ip_forward=1\n' > /etc/sysctl.d/99-kubeinit.conf",
    ]).await?;
    cmd::run_privileged("sysctl", &["--system"]).await?;
    info!("Sysctl parameters OK");

    Ok(())
}

/// Initialize the Kubernetes cluster with kubeadm.
pub async fn init_cluster(config: &ClusterConfig) -> Result<()> {
    info!(
        "Initializing cluster with control-plane endpoint: {}",
        config.control_plane_endpoint
    );

    let mut args = vec![
        "init",
        "--control-plane-endpoint",
        &config.control_plane_endpoint,
        "--pod-network-cidr",
        &config.pod_network_cidr,
        "--service-cidr",
        &config.service_cidr,
        // Cilium replaces kube-proxy
        "--skip-phases",
        "addon/kube-proxy",
    ];

    let version_flag;
    if let Some(ref v) = config.kubernetes_version {
        version_flag = format!("v{}", v.trim_start_matches('v'));
        args.push("--kubernetes-version");
        args.push(&version_flag);
    }

    cmd::run_privileged("kubeadm", &args).await?;

    // Set up kubeconfig for the current user
    setup_kubeconfig().await?;

    // Remove the control-plane NoSchedule taint so workloads (Longhorn,
    // user pods) can be scheduled on single-node or small clusters.
    let hostname = cmd::run_output("hostname", &[]).await.unwrap_or_default();
    if !hostname.is_empty() {
        info!("Removing NoSchedule taint from {hostname}...");
        cmd::run(
            "kubectl",
            &[
                "taint", "nodes", &hostname,
                "node-role.kubernetes.io/control-plane:NoSchedule-",
            ],
        )
        .await
        .ok();
    }

    info!("kubeadm init completed successfully");
    Ok(())
}

/// Copy admin kubeconfig to the invoking user's home directory, ensuring
/// the files are owned by that user rather than root.
async fn setup_kubeconfig() -> Result<()> {
    let user = cmd::real_user();
    let home = user
        .as_ref()
        .map(|u| u.home.as_str())
        .unwrap_or("/root");
    let kube_dir = format!("{home}/.kube");
    let kube_config = format!("{kube_dir}/config");
    let admin_conf = format!("{}/admin.conf", config::PATH_KUBERNETES_CONF_DIR);

    cmd::run_privileged("mkdir", &["-p", &kube_dir]).await?;
    cmd::run_privileged(
        "cp",
        &["-f", &admin_conf, &kube_config],
    )
    .await?;

    // Restrict kubeconfig to owner-only access
    cmd::run_privileged("chmod", &["600", &kube_config]).await?;

    // Fix ownership so the real user (not root) owns ~/.kube
    if let Some(ref u) = user {
        let owner = format!("{}:{}", u.uid, u.gid);
        cmd::run_privileged("chown", &["-R", &owner, &kube_dir]).await?;
        info!("kubeconfig written to {kube_config} (owner: {})", u.name);
    } else {
        info!("kubeconfig written to {kube_config}");
    }
    Ok(())
}

/// Join this node to an existing Kubernetes cluster.
pub async fn join_cluster(join_config: &JoinConfig) -> Result<()> {
    info!("Joining cluster at {} as {}...", join_config.api_server, join_config.role);

    if join_config.role == NodeRole::ControlPlane && join_config.certificate_key.is_none() {
        bail!(
            "Control-plane join requires --certificate-key. \
             Generate one with: kubeinit join-token --control-plane"
        );
    }

    let mut args = vec![
        "join",
        &join_config.api_server,
        "--token",
        &join_config.token,
        "--discovery-token-ca-cert-hash",
        &join_config.ca_cert_hash,
    ];

    let cert_key_owned;
    if join_config.role == NodeRole::ControlPlane {
        args.push("--control-plane");
        if let Some(ref key) = join_config.certificate_key {
            cert_key_owned = key.clone();
            args.push("--certificate-key");
            args.push(&cert_key_owned);
        }
    }

    cmd::run_privileged("kubeadm", &args).await?;

    // Set up kubeconfig for the joining node
    setup_kubeconfig().await?;

    info!("Successfully joined cluster as {}", join_config.role);
    Ok(())
}

/// Print a `kubeadm join` command. When `control_plane` is true, also upload
/// certificates and include `--control-plane --certificate-key` in the output.
pub async fn print_join_command(control_plane: bool) -> Result<()> {
    let join_cmd = cmd::run_privileged_output(
        "kubeadm",
        &["token", "create", "--print-join-command"],
    )
    .await?;

    if control_plane {
        // Upload certs and get a certificate key
        let cert_key = cmd::run_privileged_output(
            "kubeadm",
            &["init", "phase", "upload-certs", "--upload-certs"],
        )
        .await?;

        // The last line of the output is the certificate key
        let key = cert_key
            .lines()
            .last()
            .unwrap_or("")
            .trim();

        println!("{join_cmd} --control-plane --certificate-key {key}");

        println!();
        println!("Or use kubeinit on the joining node:");
        // Parse the join command to extract token and api-server
        let parts: Vec<&str> = join_cmd.split_whitespace().collect();
        let api_server = parts.get(2).unwrap_or(&"<api-server:6443>");
        let token = parts
            .iter()
            .position(|&t| t == "--token")
            .and_then(|i| parts.get(i + 1))
            .unwrap_or(&"<token>");
        let ca_hash = parts
            .iter()
            .position(|&t| t == "--discovery-token-ca-cert-hash")
            .and_then(|i| parts.get(i + 1))
            .unwrap_or(&"<hash>");

        println!(
            "  kubeinit join --role control-plane \\\n    \
             --api-server {api_server} \\\n    \
             --token {token} \\\n    \
             --ca-cert-hash {ca_hash} \\\n    \
             --certificate-key {key}"
        );
    } else {
        println!("{join_cmd}");

        println!();
        println!("Or use kubeinit on the joining node:");
        let parts: Vec<&str> = join_cmd.split_whitespace().collect();
        let api_server = parts.get(2).unwrap_or(&"<api-server:6443>");
        let token = parts
            .iter()
            .position(|&t| t == "--token")
            .and_then(|i| parts.get(i + 1))
            .unwrap_or(&"<token>");
        let ca_hash = parts
            .iter()
            .position(|&t| t == "--discovery-token-ca-cert-hash")
            .and_then(|i| parts.get(i + 1))
            .unwrap_or(&"<hash>");

        println!(
            "  kubeinit join --role worker \\\n    \
             --api-server {api_server} \\\n    \
             --token {token} \\\n    \
             --ca-cert-hash {ca_hash}"
        );
    }

    Ok(())
}

/// Reset the cluster — tears down all Kubernetes workloads and configuration
/// but leaves the installed binaries intact (use `uninstall` to remove those).
pub async fn reset_cluster(force: bool) -> Result<()> {
    if !force {
        info!("This will destroy the cluster. Re-run with --force to confirm.");
        return Ok(());
    }

    info!("Resetting cluster...");

    // 1. Uninstall Helm releases (Longhorn, Cilium) while the API server is up
    if cmd::binary_exists("helm").await {
        info!("Removing Helm releases...");
        cmd::run("helm", &["uninstall", "longhorn", "-n", "longhorn-system"])
            .await
            .ok();
        cmd::run("helm", &["uninstall", "cilium", "-n", "kube-system"])
            .await
            .ok();
    }

    // 2. Remove Gateway API CRDs
    if cmd::binary_exists("kubectl").await {
        let version = config::DEFAULT_GATEWAY_API_VERSION;
        let url = config::URL_GATEWAY_API_CRDS.replace("{version}", version);
        info!("Removing Gateway API CRDs...");
        cmd::run("kubectl", &["delete", "-f", &url, "--ignore-not-found"])
            .await
            .ok();
    }

    // 3. Drain this node
    if cmd::binary_exists("kubectl").await {
        let hostname = cmd::run_output("hostname", &[]).await.unwrap_or_default();
        if !hostname.is_empty() {
            info!("Draining node {hostname}...");
            cmd::run(
                "kubectl",
                &[
                    "drain", &hostname,
                    "--ignore-daemonsets",
                    "--delete-emptydir-data",
                    "--force",
                    "--grace-period=30",
                    "--timeout=60s",
                ],
            )
            .await
            .ok();
        }
    }

    // 4. kubeadm reset
    cmd::run_privileged("kubeadm", &["reset", "--force"]).await?;

    // 5. Stop kubelet
    cmd::run_privileged("systemctl", &["stop", "kubelet"]).await.ok();

    // 6. Clean up CNI config and iptables
    cmd::run_privileged("rm", &["-rf", config::PATH_CNI_CONF_DIR]).await.ok();
    cmd::run_privileged("iptables", &["-F"]).await.ok();
    cmd::run_privileged("iptables", &["-t", "nat", "-F"]).await.ok();
    cmd::run_privileged("iptables", &["-t", "mangle", "-F"]).await.ok();
    cmd::run_privileged("iptables", &["-X"]).await.ok();
    cmd::run_privileged("ipvsadm", &["-C"]).await.ok();

    // 7. Clean up Kubernetes and Longhorn data directories
    cmd::run_privileged("rm", &["-rf", config::PATH_KUBELET_DATA_DIR]).await.ok();
    cmd::run_privileged("rm", &["-rf", config::PATH_KUBERNETES_CONF_DIR]).await.ok();
    cmd::run_privileged("rm", &["-rf", "/var/lib/longhorn"]).await.ok();

    // 8. Remove kubeinit-managed sysctl and modules-load configs
    cmd::run_privileged("rm", &["-f", "/etc/modules-load.d/kubeinit.conf"]).await.ok();
    cmd::run_privileged("rm", &["-f", "/etc/sysctl.d/99-kubeinit.conf"]).await.ok();

    // 9. Clean up user kubeconfig
    if let Some(user) = cmd::real_user() {
        let kube_dir = format!("{}/.kube", user.home);
        cmd::run_privileged("rm", &["-rf", &kube_dir]).await.ok();
    }

    info!("Cluster reset complete");
    Ok(())
}

/// Print a summary of installed binary locations, credentials, and
/// configuration paths after a successful cluster initialization.
pub fn print_post_init_summary(config: &ClusterConfig) {
    let home = cmd::real_user()
        .map(|u| u.home)
        .unwrap_or_else(|| "/root".into());

    let bin = config::PATH_BIN_DIR;
    let sbin = config::PATH_SBIN_DIR;

    let binaries = [
        ("kubeadm", format!("{bin}/kubeadm")),
        ("kubelet", format!("{bin}/kubelet")),
        ("kubectl", format!("{bin}/kubectl")),
        ("crictl", format!("{bin}/crictl")),
        ("containerd", format!("{bin}/containerd")),
        ("runc", format!("{sbin}/runc")),
        ("helm", format!("{bin}/helm")),
        ("cilium", format!("{bin}/cilium")),
    ];

    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║             Cluster Initialization Summary                  ║");
    println!("╠══════════════════════════════════════════════════════════════╣");

    // Cluster info
    println!("║  Control-plane endpoint: {:<35} ║", config.control_plane_endpoint);
    println!("║  Pod network CIDR:       {:<35} ║", config.pod_network_cidr);
    println!("║  Service CIDR:           {:<35} ║", config.service_cidr);

    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Installed Binaries                                        ║");
    println!("╟──────────────────────────────────────────────────────────────╢");
    for (name, path) in &binaries {
        println!("║  {:<12} {:<46} ║", name, path.as_str());
    }

    let k8s = config::PATH_KUBERNETES_CONF_DIR;
    let kubelet_data = config::PATH_KUBELET_DATA_DIR;
    let ctrd_conf = config::PATH_CONTAINERD_CONF_DIR;
    let cni_conf = config::PATH_CNI_CONF_DIR;
    let cni_bin = config::PATH_CNI_BIN_DIR;
    let unit = config::PATH_SYSTEMD_UNIT_DIR;

    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Credentials & Certificates                                ║");
    println!("╟──────────────────────────────────────────────────────────────╢");
    let certs = [
        ("CA cert",         format!("{k8s}/pki/ca.crt")),
        ("CA key",          format!("{k8s}/pki/ca.key")),
        ("API server cert", format!("{k8s}/pki/apiserver.crt")),
        ("API server key",  format!("{k8s}/pki/apiserver.key")),
        ("SA key pair",     format!("{k8s}/pki/sa.{{key,pub}}")),
        ("Front proxy CA",  format!("{k8s}/pki/front-proxy-ca.crt")),
        ("Etcd CA",         format!("{k8s}/pki/etcd/ca.crt")),
    ];
    for (label, path) in &certs {
        println!("║  {:<17}{:<43} ║", format!("{label}:"), path);
    }

    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Configuration Files                                       ║");
    println!("╟──────────────────────────────────────────────────────────────╢");

    let configs = [
        ("Kubeconfig (user)",  format!("{home}/.kube/config")),
        ("Kubeconfig (admin)", format!("{k8s}/admin.conf")),
        ("Kubelet config",     format!("{kubelet_data}/config.yaml")),
        ("Kubelet flags",      format!("{kubelet_data}/kubeadm-flags.env")),
        ("Containerd config",  format!("{ctrd_conf}/config.toml")),
        ("CNI config dir",     format!("{cni_conf}/")),
        ("CNI plugin dir",     format!("{cni_bin}/")),
    ];
    for (label, path) in &configs {
        println!("║  {:<21}{:<39} ║", format!("{label}:"), path);
    }

    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Systemd Services                                          ║");
    println!("╟──────────────────────────────────────────────────────────────╢");
    let services = [
        ("kubelet",    format!("{unit}/kubelet.service")),
        ("containerd", format!("{unit}/containerd.service")),
    ];
    for (label, path) in &services {
        println!("║  {:<13}{:<47} ║", format!("{label}:"), path);
    }

    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Next Steps                                                ║");
    println!("╟──────────────────────────────────────────────────────────────╢");
    println!("║  Join worker nodes:  kubeinit join-token                    ║");
    println!("║  Check status:       kubeinit status                        ║");
    println!("║  Reset cluster:      kubeinit reset --force                 ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}

/// Show basic cluster status via kubectl.
pub async fn show_status() -> Result<()> {
    info!("Node status:");
    cmd::run("kubectl", &["get", "nodes", "-o", "wide"]).await?;

    println!();
    info!("Pod status (all namespaces):");
    cmd::run(
        "kubectl",
        &["get", "pods", "--all-namespaces", "-o", "wide"],
    )
    .await?;

    println!();
    info!("Cilium status:");
    if cmd::binary_exists("cilium").await {
        cmd::run("cilium", &["status"]).await.ok();
    } else {
        cmd::run(
            "kubectl",
            &[
                "get", "pods",
                "-n", "kube-system",
                "-l", "app.kubernetes.io/name=cilium-agent",
                "-o", "wide",
            ],
        )
        .await?;
    }

    Ok(())
}
