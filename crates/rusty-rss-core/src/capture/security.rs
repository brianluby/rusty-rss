//! SSRF protections for outbound capture: a DNS-rebinding-safe HTTP client,
//! URL validation, and private/loopback IP blocking.

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, redirect::Policy};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use url::Url;

pub fn build_capture_client(user_agent: &str, allow_private_hosts: bool) -> Client {
    Client::builder()
        .user_agent(user_agent)
        .redirect(Policy::none())
        .dns_resolver(Arc::new(GuardedResolver {
            allow_private_hosts,
        }))
        .build()
        .expect("reqwest client build should not fail")
}

/// DNS resolver that drops private/loopback addresses at resolution time, so the
/// address the client actually connects to is the one that passed the check.
///
/// The standalone [`validate_capture_url`] pre-check resolves the host once, but
/// reqwest resolves again when it connects; an attacker-controlled name can
/// return a public IP to the pre-check and a private IP to the connection (DNS
/// rebinding). Enforcing the policy inside the resolver the client connects with
/// closes that window. Literal-IP URLs bypass DNS entirely and are guarded by
/// [`validate_capture_url`] instead.
#[derive(Debug, Clone, Copy)]
struct GuardedResolver {
    allow_private_hosts: bool,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_private_hosts = self.allow_private_hosts;
        Box::pin(async move {
            // Port 0 is a placeholder; reqwest's connector applies the real port.
            let resolved = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let allowed = allowed_resolved_addrs(resolved, allow_private_hosts);
            if allowed.is_empty() {
                return Err(Box::<dyn std::error::Error + Send + Sync>::from(
                    "blocked private outbound host",
                ));
            }
            let addrs: reqwest::dns::Addrs = Box::new(allowed.into_iter());
            Ok(addrs)
        })
    }
}

/// Keep only the addresses the client is allowed to connect to. This is the
/// policy the connection uses, so a name that resolves to a mix of public and
/// private IPs only ever connects to the public ones.
fn allowed_resolved_addrs(
    resolved: impl Iterator<Item = SocketAddr>,
    allow_private_hosts: bool,
) -> Vec<SocketAddr> {
    resolved
        .filter(|addr| allow_private_hosts || !is_blocked_ip(addr.ip()))
        .collect()
}

pub(super) async fn validate_capture_url(url: &str, allow_private_hosts: bool) -> Result<()> {
    let parsed = Url::parse(url).context("invalid outbound URL")?;
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(anyhow!("unsupported outbound URL scheme: {scheme}")),
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("outbound URL is missing a host"))?;
    if host.eq_ignore_ascii_case("localhost") && !allow_private_hosts {
        return Err(anyhow!("blocked private outbound host"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !allow_private_hosts && is_blocked_ip(ip) {
            return Err(anyhow!("blocked private outbound host"));
        }
        return Ok(());
    }
    if allow_private_hosts {
        return Ok(());
    }

    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .context("failed to resolve outbound host")?;
    for addr in addrs {
        if is_blocked_ip(addr.ip()) {
            return Err(anyhow!("blocked private outbound host"));
        }
    }

    Ok(())
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    // Normalize IPv4-mapped IPv6 (e.g. ::ffff:127.0.0.1) to IPv4 so an embedded
    // private/loopback address cannot slip through the IPv6 branch.
    let ip = match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        other => other,
    };
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                // CGNAT / shared address space, 100.64.0.0/10.
                || (a == 100 && (64..=127).contains(&b))
                // Benchmarking, 198.18.0.0/15.
                || (a == 198 && (b == 18 || b == 19))
                // Reserved (incl. future use), 240.0.0.0/4.
                || a >= 240
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            let first = segments[0];
            ip.is_loopback()
                || ip.is_unspecified()
                // Unique local addresses, fc00::/7.
                || (first & 0xfe00) == 0xfc00
                // Link-local unicast, fe80::/10 (the old 0xfe00 mask never matched).
                || (first & 0xffc0) == 0xfe80
                // Multicast, ff00::/8.
                || (first & 0xff00) == 0xff00
                // Documentation, 2001:db8::/32.
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn validate_capture_url_blocks_private_hosts_by_default() {
        let err = validate_capture_url("http://127.0.0.1:8080/private", false)
            .await
            .expect_err("private host should be blocked");

        assert!(err.to_string().contains("blocked private outbound host"));
    }

    #[test]
    fn allowed_resolved_addrs_drops_private_hosts() {
        let public: SocketAddr = "1.1.1.1:0".parse().expect("addr");
        let private: SocketAddr = "10.0.0.5:0".parse().expect("addr");
        let loopback: SocketAddr = "127.0.0.1:0".parse().expect("addr");

        // Default policy keeps only the public address, so the connection can
        // never reach the private/loopback ones even if DNS returns them.
        let allowed = allowed_resolved_addrs([public, private, loopback].into_iter(), false);
        assert_eq!(allowed, vec![public]);

        // No public address resolves -> empty -> the resolver fails closed.
        let none = allowed_resolved_addrs([private, loopback].into_iter(), false);
        assert!(none.is_empty());

        // Opt-in allows everything (used for the localhost test servers).
        let all = allowed_resolved_addrs([public, private, loopback].into_iter(), true);
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn is_blocked_ip_covers_ipv6_edge_cases() {
        let blocked = [
            "::ffff:127.0.0.1", // IPv4-mapped loopback
            "::ffff:10.0.0.1",  // IPv4-mapped private
            "::1",              // IPv6 loopback
            "fe80::1",          // link-local fe80::/10
            "fc00::1",          // unique local fc00::/7
            "ff02::1",          // IPv6 multicast ff00::/8
            "2001:db8::1",      // IPv6 documentation 2001:db8::/32
            "100.64.0.1",       // CGNAT 100.64.0.0/10
            "198.18.0.1",       // benchmarking 198.18.0.0/15
            "240.0.0.1",        // reserved 240.0.0.0/4
            "224.0.0.1",        // IPv4 multicast 224.0.0.0/4
        ];
        for addr in blocked {
            assert!(
                is_blocked_ip(addr.parse().expect("addr")),
                "{addr} should be blocked"
            );
        }

        let allowed = [
            "1.1.1.1",              // public IPv4
            "2606:4700:4700::1111", // public IPv6
            "::ffff:1.1.1.1",       // IPv4-mapped public
        ];
        for addr in allowed {
            assert!(
                !is_blocked_ip(addr.parse().expect("addr")),
                "{addr} should be allowed"
            );
        }
    }
}
