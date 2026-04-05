mod cluster;
mod cmd;
mod cni;
mod config;
mod deps;
mod net;

use std::io::{self, BufRead, Write};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kubeinit", version, about = "Initialize vanilla Kubernetes clusters with kubeadm and Cilium CNI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new Kubernetes cluster
    Init {
        /// Control plane endpoint address (IP or hostname). Auto-detected if omitted.
        #[arg(long)]
        control_plane_endpoint: Option<String>,

        /// Pod network CIDR
        #[arg(long, default_value = config::DEFAULT_POD_CIDR)]
        pod_network_cidr: String,

        /// Service CIDR
        #[arg(long, default_value = config::DEFAULT_SERVICE_CIDR)]
        service_cidr: String,

        /// Kubernetes version to install (e.g. 1.31.0)
        #[arg(long)]
        kubernetes_version: Option<String>,

        /// Cilium version to install (e.g. 1.16.0)
        #[arg(long)]
        cilium_version: Option<String>,

        /// Skip Cilium CNI installation
        #[arg(long, default_value_t = false)]
        skip_cni: bool,
    },
    /// Generate a join command for worker nodes
    JoinToken,
    /// Check prerequisites for cluster initialization
    Preflight,
    /// Reset the cluster (destroy)
    Reset {
        /// Skip confirmation prompt
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Install all required dependencies (containerd, kubeadm, kubelet, kubectl, crictl, helm, cilium CLI)
    InstallDeps {
        /// Kubernetes version (e.g. 1.32.3)
        #[arg(long)]
        kubernetes_version: Option<String>,

        /// Cilium CLI version (e.g. 0.16.24)
        #[arg(long)]
        cilium_version: Option<String>,
    },
    /// Remove all cluster dependencies (containerd, kubeadm, kubelet, kubectl, crictl, helm, cilium CLI, runc, CNI plugins)
    Uninstall {
        /// Skip confirmation prompt
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Show cluster status
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init {
            control_plane_endpoint,
            pod_network_cidr,
            service_cidr,
            kubernetes_version,
            cilium_version,
            skip_cni,
        } => {
            deps::install_all(
                kubernetes_version.as_deref(),
                cilium_version.as_deref(),
            )
            .await?;

            let control_plane_endpoint = resolve_control_plane_endpoint(
                control_plane_endpoint,
            )
            .await?;

            let config = cluster::ClusterConfig {
                control_plane_endpoint,
                pod_network_cidr: pod_network_cidr.clone(),
                service_cidr,
                kubernetes_version,
            };

            cluster::preflight_checks().await?;
            cluster::init_cluster(&config).await?;

            if !skip_cni {
                let cni_config = cni::CiliumConfig {
                    version: cilium_version,
                    pod_cidr: pod_network_cidr,
                };
                cni::install_cilium(&cni_config).await?;
            }

            cluster::print_post_init_summary(&config);
            tracing::info!("Cluster initialization complete!");
        }
        Commands::JoinToken => {
            cluster::print_join_command().await?;
        }
        Commands::Preflight => {
            cluster::preflight_checks().await?;
            tracing::info!("All preflight checks passed");
        }
        Commands::Reset { force } => {
            cluster::reset_cluster(force).await?;
        }
        Commands::InstallDeps {
            kubernetes_version,
            cilium_version,
        } => {
            deps::install_all(
                kubernetes_version.as_deref(),
                cilium_version.as_deref(),
            )
            .await?;
        }
        Commands::Uninstall { force } => {
            if !force {
                eprintln!("This will remove all Kubernetes cluster components and related tooling.");
                eprint!("Are you sure? [y/N]: ");
                io::stderr().flush()?;

                let mut input = String::new();
                io::stdin().lock().read_line(&mut input)?;
                let input = input.trim();

                if !matches!(input, "y" | "Y" | "yes" | "Yes") {
                    bail!("Aborted.");
                }
            }
            deps::uninstall_all().await?;
        }
        Commands::Status => {
            cluster::show_status().await?;
        }
    }

    Ok(())
}

/// If the user supplied `--control-plane-endpoint`, use it directly.
/// Otherwise detect the host's default IP and hostname and ask for confirmation.
async fn resolve_control_plane_endpoint(explicit: Option<String>) -> Result<String> {
    if let Some(ep) = explicit {
        return Ok(ep);
    }

    let ip = net::detect_default_ip().await?;
    let hostname = net::detect_hostname().await.unwrap_or_default();

    let detected = if hostname.is_empty() {
        ip.to_string()
    } else {
        format!("{hostname} ({ip})")
    };

    eprintln!("Detected control-plane endpoint: {detected}");
    eprint!("Use this as the control-plane endpoint? [Y/n/custom value]: ");
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().lock().read_line(&mut input)?;
    let input = input.trim();

    match input {
        "" | "y" | "Y" | "yes" | "Yes" => Ok(ip.to_string()),
        "n" | "N" | "no" | "No" => {
            bail!("Aborted. Re-run with --control-plane-endpoint to specify one.")
        }
        custom => Ok(custom.to_string()),
    }
}
