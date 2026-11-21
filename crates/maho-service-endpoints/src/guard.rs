//! The endpoint guard: which origins a service call may go to, and which hosts may carry plain text.
//!
//! A service origin is HTTPS. Plain HTTP is allowed only toward a host whose traffic never leaves the
//! machine (loopback) or rides an encrypted, authenticated overlay (a Tailscale tailnet: the CGNAT
//! range `100.64.0.0/10`, the tailnet IPv6 range `fd7a:115c:a1e0::/48` and `*.ts.net` names). An
//! ordinary private network (`192.168.0.0/16`, `10.0.0.0/8`, ...) is not trusted with plain text.

use std::fmt;
use std::net::IpAddr;

/// Why an endpoint is refused before anything is sent to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointRefusal {
    /// The value is not an absolute URL.
    Unparsable,
    /// A scheme other than `https` and `http`.
    UnsupportedScheme(String),
    /// Plain `http` toward a host that is neither loopback nor on a tailnet.
    InsecureHost(String),
    /// A user name or password in the URL.
    Credentials,
    /// No host.
    MissingHost,
    /// A path, query or fragment where only an origin is allowed.
    NotAnOrigin,
}

impl EndpointRefusal {
    /// A stable machine code for the refusal.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unparsable => "endpoint_unparsable",
            Self::UnsupportedScheme(_) => "endpoint_scheme",
            Self::InsecureHost(_) => "endpoint_insecure",
            Self::Credentials => "endpoint_credentials",
            Self::MissingHost => "endpoint_no_host",
            Self::NotAnOrigin => "endpoint_not_origin",
        }
    }
}

impl fmt::Display for EndpointRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparsable => formatter.write_str("expected an absolute https:// service origin"),
            Self::UnsupportedScheme(scheme) => {
                write!(
                    formatter,
                    "the {scheme}: scheme is not allowed; use an https:// origin"
                )
            }
            Self::InsecureHost(host) => write!(
                formatter,
                "plain http to {host} is refused; use https, or a loopback or tailnet host"
            ),
            Self::Credentials => formatter
                .write_str("credentials in the URL are refused; expected an HTTPS service origin"),
            Self::MissingHost => formatter.write_str("the URL names no host"),
            Self::NotAnOrigin => formatter
                .write_str("expected an HTTPS service origin, without a path, query or fragment"),
        }
    }
}

impl std::error::Error for EndpointRefusal {}

/// `127.0.0.0/8`, `::1`, and their IPv4-mapped forms.
pub fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// The Tailscale ranges: `100.64.0.0/10` and `fd7a:115c:a1e0::/48` (and the IPv4-mapped form).
pub fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            octets[0] == 100 && octets[1] & 0xc0 == 0x40
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            (segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0)
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_tailnet_ip(IpAddr::V4(v4)))
        }
    }
}

fn normalized(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn parse_ip(host: &str) -> Option<IpAddr> {
    host.parse().ok()
}

/// `localhost`, `*.localhost`, or a loopback address (brackets around IPv6 allowed).
pub fn is_loopback_host(host: &str) -> bool {
    let host = normalized(host);
    host == "localhost"
        || host.ends_with(".localhost")
        || parse_ip(&host).is_some_and(is_loopback_ip)
}

/// A MagicDNS name (`<machine>.<tailnet>.ts.net`) or a tailnet address.
pub fn is_tailnet_host(host: &str) -> bool {
    let host = normalized(host);
    let name = host.strip_suffix(".ts.net").filter(|name| !name.is_empty());
    name.is_some() || parse_ip(&host).is_some_and(is_tailnet_ip)
}

/// Whether plain text (`http`, an unencrypted peer link) may go to `host`.
pub fn allows_plain_text(host: &str) -> bool {
    is_loopback_host(host) || is_tailnet_host(host)
}

/// The origin a service call may use, as `scheme://host[:port]`.
pub fn check_service_origin(value: &str) -> Result<String, EndpointRefusal> {
    let url = url::Url::parse(value.trim()).map_err(|_| EndpointRefusal::Unparsable)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointRefusal::Credentials);
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or(EndpointRefusal::MissingHost)?
        .to_owned();
    match url.scheme() {
        "https" => {}
        "http" if allows_plain_text(&host) => {}
        "http" => return Err(EndpointRefusal::InsecureHost(host)),
        other => return Err(EndpointRefusal::UnsupportedScheme(other.to_owned())),
    }
    if url.query().is_some() || url.fragment().is_some() || url.path() != "/" {
        return Err(EndpointRefusal::NotAnOrigin);
    }
    Ok(url.origin().ascii_serialization())
}

/// A peer device's host for a link that carries plain text: loopback or tailnet only.
pub fn check_peer_host(host: &str) -> Result<(), EndpointRefusal> {
    if host.trim().is_empty() {
        return Err(EndpointRefusal::MissingHost);
    }
    if allows_plain_text(host) {
        Ok(())
    } else {
        Err(EndpointRefusal::InsecureHost(normalized(host)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailnet_range_is_exactly_the_cgnat_block() {
        assert!(is_tailnet_host("100.64.0.1"));
        assert!(is_tailnet_host("100.127.255.254"));
        assert!(!is_tailnet_host("100.63.255.255"));
        assert!(!is_tailnet_host("100.128.0.1"));
        assert!(is_tailnet_host("[fd7a:115c:a1e0::1]"));
        assert!(!is_tailnet_host("fd7a:115c:a1e1::1"));
        assert!(is_tailnet_host("box.tail1234.ts.net."));
        assert!(!is_tailnet_host(".ts.net"));
        assert!(!is_tailnet_host("evil-ts.net"));
    }

    #[test]
    fn loopback_names_and_mapped_addresses() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("api.localhost"));
        assert!(is_loopback_host("127.0.0.53"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("::ffff:127.0.0.1"));
        assert!(!is_loopback_host("localhost.example.com"));
    }
}
