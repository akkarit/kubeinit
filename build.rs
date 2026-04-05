use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo::rerun-if-changed=versions.toml");
    println!("cargo::rerun-if-changed=config.toml");

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = env::var("OUT_DIR").unwrap();

    let mut code = String::new();

    // ── versions.toml ────────────────────────────────────────────────────────

    let versions_content = fs::read_to_string(Path::new(&manifest_dir).join("versions.toml"))
        .expect("failed to read versions.toml");
    let versions: HashMap<String, HashMap<String, String>> =
        toml::from_str(&versions_content).expect("failed to parse versions.toml");

    let version_mapping = [
        ("kubernetes", "DEFAULT_KUBERNETES_VERSION"),
        ("containerd", "DEFAULT_CONTAINERD_VERSION"),
        ("runc", "DEFAULT_RUNC_VERSION"),
        ("cni-plugins", "DEFAULT_CNI_PLUGINS_VERSION"),
        ("crictl", "DEFAULT_CRICTL_VERSION"),
        ("helm", "DEFAULT_HELM_VERSION"),
        ("cilium-cli", "DEFAULT_CILIUM_CLI_VERSION"),
        ("gateway-api", "DEFAULT_GATEWAY_API_VERSION"),
        ("longhorn", "DEFAULT_LONGHORN_VERSION"),
    ];

    code.push_str("// Generated from versions.toml\n");
    for (key, const_name) in &version_mapping {
        let version = versions
            .get(*key)
            .and_then(|t| t.get("version"))
            .unwrap_or_else(|| panic!("missing [{key}].version in versions.toml"));
        code.push_str(&format!("pub const {const_name}: &str = \"{version}\";\n"));
    }

    // ── config.toml ──────────────────────────────────────────────────────────

    let config_content = fs::read_to_string(Path::new(&manifest_dir).join("config.toml"))
        .expect("failed to read config.toml");
    let config: toml::Value =
        toml::from_str(&config_content).expect("failed to parse config.toml");

    // Network defaults
    code.push_str("\n// Generated from config.toml [network]\n");
    emit_str(&mut code, "DEFAULT_POD_CIDR", &config, &["network", "pod_cidr"]);
    emit_str(&mut code, "DEFAULT_SERVICE_CIDR", &config, &["network", "service_cidr"]);

    // Runtime
    code.push_str("\n// Generated from config.toml [runtime]\n");
    emit_str(&mut code, "CONTAINERD_SOCKET", &config, &["runtime", "containerd_socket"]);

    // Paths
    code.push_str("\n// Generated from config.toml [paths]\n");
    let path_mapping = [
        ("bin_dir", "PATH_BIN_DIR"),
        ("sbin_dir", "PATH_SBIN_DIR"),
        ("cni_bin_dir", "PATH_CNI_BIN_DIR"),
        ("cni_conf_dir", "PATH_CNI_CONF_DIR"),
        ("containerd_conf_dir", "PATH_CONTAINERD_CONF_DIR"),
        ("kubernetes_conf_dir", "PATH_KUBERNETES_CONF_DIR"),
        ("kubelet_data_dir", "PATH_KUBELET_DATA_DIR"),
        ("etcd_data_dir", "PATH_ETCD_DATA_DIR"),
        ("containerd_data_dir", "PATH_CONTAINERD_DATA_DIR"),
        ("containerd_run_dir", "PATH_CONTAINERD_RUN_DIR"),
        ("systemd_unit_dir", "PATH_SYSTEMD_UNIT_DIR"),
        ("crictl_config", "PATH_CRICTL_CONFIG"),
    ];
    for (key, const_name) in &path_mapping {
        emit_str(&mut code, const_name, &config, &["paths", key]);
    }

    // URLs
    code.push_str("\n// Generated from config.toml [urls]\n");
    let url_mapping = [
        ("containerd", "URL_CONTAINERD"),
        ("containerd_service", "URL_CONTAINERD_SERVICE"),
        ("runc", "URL_RUNC"),
        ("cni_plugins", "URL_CNI_PLUGINS"),
        ("crictl", "URL_CRICTL"),
        ("kubernetes", "URL_KUBERNETES"),
        ("helm", "URL_HELM"),
        ("cilium_cli", "URL_CILIUM_CLI"),
        ("cilium_helm_repo", "URL_CILIUM_HELM_REPO"),
        ("gateway_api_crds", "URL_GATEWAY_API_CRDS"),
        ("longhorn_helm_repo", "URL_LONGHORN_HELM_REPO"),
    ];
    for (key, const_name) in &url_mapping {
        emit_str(&mut code, const_name, &config, &["urls", key]);
    }

    fs::write(Path::new(&out_dir).join("generated_config.rs"), code)
        .expect("failed to write generated_config.rs");
}

fn emit_str(code: &mut String, const_name: &str, config: &toml::Value, path: &[&str]) {
    let mut val = config;
    for key in path {
        val = val
            .get(key)
            .unwrap_or_else(|| panic!("missing key {key} in config.toml path {path:?}"));
    }
    let s = val
        .as_str()
        .unwrap_or_else(|| panic!("expected string at {path:?} in config.toml"));
    code.push_str(&format!("pub const {const_name}: &str = \"{s}\";\n"));
}
