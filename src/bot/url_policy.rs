use std::net::{IpAddr, SocketAddr};
use url::{Host, Url};

pub(crate) struct ResolvedDownloadUrl {
    pub url: Url,
    pub host: String,
    pub address: SocketAddr,
}

fn is_unsafe_remote_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

fn parse_download_url(raw: &str) -> Result<Url, String> {
    let parsed = Url::parse(raw.trim()).map_err(|_| "invalid external media URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("external media URL must use http or https with a host".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("external media URL must not contain embedded credentials".to_string());
    }
    if let Some(host) = parsed.host() {
        let literal_ip = match host {
            Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
            Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
            Host::Domain(_) => None,
        };
        if literal_ip.is_some_and(is_unsafe_remote_ip) {
            return Err("external media URL points to a blocked network address".to_string());
        }
    }
    Ok(parsed)
}

pub(crate) async fn resolve_download_url(raw: &str) -> Result<ResolvedDownloadUrl, String> {
    let url = parse_download_url(raw)?;
    let host = url
        .host_str()
        .ok_or_else(|| "external media URL has no host".to_string())?
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "external media URL has no usable port".to_string())?;

    let resolved = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| "external media host could not be resolved".to_string())?
        .collect::<Vec<_>>();

    if resolved.is_empty() || resolved.iter().any(|addr| is_unsafe_remote_ip(addr.ip())) {
        return Err("external media URL resolved to a blocked network address".to_string());
    }

    Ok(ResolvedDownloadUrl {
        url,
        host,
        address: resolved[0],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_schemes_credentials_and_literal_private_ips() {
        assert!(parse_download_url("file:///etc/passwd").is_err());
        assert!(parse_download_url("ftp://example.com/file").is_err());
        assert!(parse_download_url("https://user:pass@example.com/file").is_err());
        assert!(parse_download_url("http://127.0.0.1/file").is_err());
        assert!(parse_download_url("http://10.0.0.1/file").is_err());
        assert!(parse_download_url("http://169.254.1.1/file").is_err());
        assert!(parse_download_url("http://[::1]/file").is_err());
    }

    #[test]
    fn accepts_public_http_and_https_targets_before_dns_resolution() {
        assert!(parse_download_url("https://example.com/file").is_ok());
        assert!(parse_download_url("http://1.1.1.1/file").is_ok());
    }

    #[test]
    fn remote_ip_policy_matches_private_and_public_boundaries() {
        assert!(is_unsafe_remote_ip("192.168.1.1".parse().unwrap()));
        assert!(is_unsafe_remote_ip("fc00::1".parse().unwrap()));
        assert!(!is_unsafe_remote_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_unsafe_remote_ip(
            "2606:4700:4700::1111".parse().unwrap()
        ));
    }
}
