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

    // Check required kernel modules
    let modules_loaded = cmd::run_output("lsmod", &[]).await.unwrap_or_default();
    for module in ["br_netfilter", "overlay"] {
        if !modules_loaded.contains(module) {
            bail!(
                "Required kernel module not loaded: {module}. Load it with: modprobe {module}"
            );
        }
    }
    info!("Required kernel modules loaded");

    // Check sysctl settings
    for param in [
        "net.bridge.bridge-nf-call-iptables",
        "net.ipv4.ip_forward",
    ] {
        let val = cmd::run_output("sysctl", &["-n", param])
            .await
            .unwrap_or_default();
        if val.trim() != "1" {
            bail!("sysctl {param} must be set to 1");
        }
    }
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

    info!("kubeadm init completed successfully");
    Ok(())
}

/// Copy admin kubeconfig to the user's home directory.
async fn setup_kubeconfig() -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let kube_dir = format!("{home}/.kube");

    let admin_conf = format!("{}/admin.conf", config::PATH_KUBERNETES_CONF_DIR);

    cmd::run_privileged("mkdir", &["-p", &kube_dir]).await?;
    cmd::run_privileged(
        "cp",
        &["-f", &admin_conf, &format!("{kube_dir}/config")],
    )
    .await?;

    // Fix ownership if running under sudo
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        cmd::run_privileged(
            "chown",
            &["-R", &format!("{sudo_user}:{sudo_user}"), &kube_dir],
        )
        .await?;
    }

    info!("kubeconfig written to {kube_dir}/config");
    Ok(())
}

/// Print a `kubeadm join` command that can be used on worker nodes.
pub async fn print_join_command() -> Result<()> {
    let token = cmd::run_privileged_output("kubeadm", &["token", "create", "--print-join-command"]).await?;
    println!("{token}");
    Ok(())
}

/// Reset the cluster using `kubeadm reset`.
pub async fn reset_cluster(force: bool) -> Result<()> {
    if !force {
        info!("This will destroy the cluster. Re-run with --force to confirm.");
        return Ok(());
    }

    info!("Resetting cluster...");
    cmd::run_privileged("kubeadm", &["reset", "--force"]).await?;

    // Clean up CNI config and iptables
    cmd::run_privileged("rm", &["-rf", config::PATH_CNI_CONF_DIR]).await.ok();
    cmd::run_privileged("iptables", &["-F"]).await.ok();
    cmd::run_privileged("iptables", &["-t", "nat", "-F"]).await.ok();
    cmd::run_privileged("iptables", &["-t", "mangle", "-F"]).await.ok();
    cmd::run_privileged("iptables", &["-X"]).await.ok();

    info!("Cluster reset complete");
    Ok(())
}

/// Print a summary of installed binary locations, credentials, and
/// configuration paths after a successful cluster initialization.
pub fn print_post_init_summary(config: &ClusterConfig) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());

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
