use std::net::IpAddr;

use anyhow::{Result, bail};

/// Detect the default non-loopback IPv4 address of this host by reading the
/// routing table. This picks the address on the interface that owns the default
/// route, which is the most likely candidate for a control-plane endpoint.
pub async fn detect_default_ip() -> Result<IpAddr> {
    // `ip -4 route show default` gives something like:
    //   default via 192.168.1.1 dev eth0 proto static metric 100
    // We extract the device name and then look up its address.
    let route = crate::cmd::run_output("ip", &["-4", "route", "show", "default"]).await?;

    let dev = route
        .split_whitespace()
        .skip_while(|t| *t != "dev")
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("could not determine default route device"))?
        .to_string();

    // `ip -4 -o addr show dev <device>` gives something like:
    //   2: eth0    inet 192.168.1.100/24 brd 192.168.1.255 scope global eth0
    let addr_output =
        crate::cmd::run_output("ip", &["-4", "-o", "addr", "show", "dev", &dev]).await?;

    for line in addr_output.lines() {
        let mut tokens = line.split_whitespace();
        if let Some(cidr) = tokens.find(|t| *t == "inet").and_then(|_| tokens.next()) {
            let ip_str = cidr.split('/').next().unwrap_or(cidr);
            if let Ok(ip) = ip_str.parse::<IpAddr>() {
                return Ok(ip);
            }
        }
    }

    bail!("could not detect an IPv4 address on device {dev}");
}

/// Detect the system hostname.
pub async fn detect_hostname() -> Result<String> {
    // Try FQDN first, fall back to short hostname
    let fqdn = crate::cmd::run_output("hostname", &["-f"]).await;
    if let Ok(h) = fqdn
        && !h.is_empty()
    {
        return Ok(h);
    }
    crate::cmd::run_output("hostname", &[]).await
}
