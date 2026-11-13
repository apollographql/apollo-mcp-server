//! Proxies whose `X-Forwarded-For` header the server believes.

use std::net::{IpAddr, SocketAddr};

use http::{HeaderMap, HeaderName};
use ipnet::{IpNet, Ipv4Net};
use schemars::JsonSchema;
use serde::Deserialize;

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// The reverse proxies and load balancers in front of the server, as CIDR
/// ranges or single IP addresses. A request from one of them may name the
/// original client in `X-Forwarded-For`; any other caller's header is ignored.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(try_from = "Vec<String>")]
pub struct TrustedProxies(#[schemars(with = "Vec<String>")] Vec<IpNet>);

/// An invalid `transport.trusted_proxies` entry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustedProxiesError {
    #[error("`{0}` is not an IP address or CIDR range")]
    InvalidEntry(String),
}

impl TryFrom<Vec<String>> for TrustedProxies {
    type Error = TrustedProxiesError;

    fn try_from(entries: Vec<String>) -> Result<Self, Self::Error> {
        entries
            .into_iter()
            .map(|entry| {
                entry
                    .parse::<IpNet>()
                    .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                    .map(to_canonical)
                    .map_err(|_| TrustedProxiesError::InvalidEntry(entry))
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }
}

/// Turns an IPv4-mapped IPv6 range, such as `::ffff:10.0.4.0/120`, into its
/// IPv4 range, because peers and hops are compared in canonical form.
fn to_canonical(net: IpNet) -> IpNet {
    let IpNet::V6(v6) = net else { return net };
    match (v6.addr().to_ipv4_mapped(), v6.prefix_len().checked_sub(96)) {
        (Some(v4), Some(prefix_len)) => Ipv4Net::new(v4, prefix_len).map_or(net, IpNet::V4),
        _ => net,
    }
}

impl TrustedProxies {
    /// The first range that trusts every peer, such as `0.0.0.0/0`, which
    /// lets any caller choose its recorded address.
    fn catch_all(&self) -> Option<IpNet> {
        self.0.iter().copied().find(|net| net.prefix_len() == 0)
    }

    /// Warns at startup about a list that trusts every peer, or that has no
    /// effect because nothing uses client addresses.
    pub(crate) fn warn_if_misconfigured(&self, client_address_used: bool) {
        if let Some(catch_all) = self.catch_all() {
            tracing::warn!(
                "transport.trusted_proxies includes {catch_all}, which trusts every peer, so any caller can choose its client address with X-Forwarded-For, both for spans and for transport.auth.rate_limit.per_client"
            );
        }
        if !self.0.is_empty() && !client_address_used {
            tracing::warn!(
                "transport.trusted_proxies has no effect because neither telemetry.exporters.tracing.client_address nor transport.auth.rate_limit.per_client is enabled"
            );
        }
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|net| net.contains(&ip))
    }

    /// The original client of a request from `peer`, when a trusted proxy
    /// names one. Reads `X-Forwarded-For` from the right, where the nearest
    /// proxy appended, skipping trusted hops. The first untrusted hop is the
    /// client. An unreadable hop ends the walk at the last trusted one, since
    /// anything to its left may be forged. When every hop is trusted, the
    /// leftmost one is the client, because it's the farthest from the server,
    /// such as an internal caller inside a trusted range.
    pub(crate) fn forwarded_client(&self, peer: IpAddr, headers: &HeaderMap) -> Option<IpAddr> {
        if !self.trusts(peer.to_canonical()) {
            return None;
        }
        let hops = headers
            .get_all(X_FORWARDED_FOR)
            .iter()
            .rev()
            .flat_map(|line| line.as_bytes().rsplit(|&byte| byte == b','))
            .map(parse_hop);
        let mut client = None;
        for hop in hops {
            let Some(hop) = hop else { break };
            client = Some(hop);
            if !self.trusts(hop) {
                break;
            }
        }
        client
    }
}

/// An `X-Forwarded-For` entry: an IP address, optionally with a port, which
/// some proxies append.
fn parse_hop(hop: &[u8]) -> Option<IpAddr> {
    let hop = std::str::from_utf8(hop).ok()?.trim();
    hop.parse::<IpAddr>()
        .or_else(|_| hop.parse::<SocketAddr>().map(|address| address.ip()))
        .ok()
        .map(|ip| ip.to_canonical())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted(entries: &[&str]) -> TrustedProxies {
        TrustedProxies::try_from(entries.iter().map(ToString::to_string).collect::<Vec<_>>())
            .expect("valid trusted proxies")
    }

    fn forwarded_client(peer: &str, lines: &[&str]) -> Option<String> {
        let mut headers = HeaderMap::new();
        for line in lines {
            headers.append(X_FORWARDED_FOR, line.parse().expect("header value"));
        }
        trusted(&["10.0.0.0/8", "2001:db8::/32"])
            .forwarded_client(peer.parse().expect("peer"), &headers)
            .map(|ip| ip.to_string())
    }

    #[rstest::rstest]
    #[case::untrusted_peer("198.51.100.1", &["203.0.113.7"], None)]
    #[case::no_header("10.0.0.1", &[], None)]
    #[case::single_hop("10.0.0.1", &["203.0.113.7"], Some("203.0.113.7"))]
    #[case::spoofed_left_of_the_client("10.0.0.1", &["1.1.1.1, 203.0.113.7"], Some("203.0.113.7"))]
    #[case::skips_trusted_hops("10.0.0.1", &["203.0.113.7, 10.0.0.2, 10.0.0.3"], Some("203.0.113.7"))]
    #[case::joins_header_lines("10.0.0.1", &["1.1.1.1", "203.0.113.7, 10.0.0.2"], Some("203.0.113.7"))]
    #[case::all_trusted("10.0.0.1", &["10.0.0.2, 10.0.0.3"], Some("10.0.0.2"))]
    #[case::unreadable_hop("10.0.0.1", &["203.0.113.7, unknown, 10.0.0.2"], Some("10.0.0.2"))]
    #[case::unreadable_last_hop("10.0.0.1", &["203.0.113.7, unknown"], None)]
    #[case::empty_hop("10.0.0.1", &["203.0.113.7, , 10.0.0.2"], Some("10.0.0.2"))]
    #[case::empty_last_hop("10.0.0.1", &["203.0.113.7, "], None)]
    #[case::hop_with_port("10.0.0.1", &["203.0.113.7:51234"], Some("203.0.113.7"))]
    #[case::ipv6_hop_with_port("10.0.0.1", &["[2001:db9::7]:51234"], Some("2001:db9::7"))]
    #[case::mapped_peer("::ffff:10.0.0.1", &["203.0.113.7"], Some("203.0.113.7"))]
    #[case::mapped_hop("10.0.0.1", &["::ffff:203.0.113.7"], Some("203.0.113.7"))]
    #[case::ipv6_peer("2001:db8::1", &["203.0.113.7"], Some("203.0.113.7"))]
    fn resolves_the_forwarded_client(
        #[case] peer: &str,
        #[case] lines: &[&str],
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(forwarded_client(peer, lines).as_deref(), expected);
    }

    #[test]
    fn an_unreadable_header_line_ends_the_walk() {
        let mut headers = HeaderMap::new();
        headers.append(X_FORWARDED_FOR, "203.0.113.7".parse().expect("header"));
        headers.append(
            X_FORWARDED_FOR,
            http::HeaderValue::from_bytes(b"\xff").expect("opaque header"),
        );

        let client =
            trusted(&["10.0.0.0/8"]).forwarded_client("10.0.0.1".parse().expect("peer"), &headers);

        assert_eq!(client, None);
    }

    #[test]
    fn an_unreadable_hop_keeps_the_hops_appended_after_it() {
        let mut headers = HeaderMap::new();
        headers.append(
            X_FORWARDED_FOR,
            http::HeaderValue::from_bytes(b"\x80, 203.0.113.7").expect("opaque header"),
        );

        let client =
            trusted(&["10.0.0.0/8"]).forwarded_client("10.0.0.1".parse().expect("peer"), &headers);

        assert_eq!(client, Some("203.0.113.7".parse().expect("client")));
    }

    #[rstest::rstest]
    #[case::cidr("10.0.0.0/8", "10.1.2.3")]
    #[case::bare_ipv4("192.168.1.10", "192.168.1.10")]
    #[case::bare_ipv6("2001:db8::1", "2001:db8::1")]
    #[case::mapped_cidr("::ffff:10.0.4.0/120", "10.0.4.7")]
    #[case::mapped_ip("::ffff:10.0.4.7", "10.0.4.7")]
    fn trusts_each_configured_entry(#[case] entry: &str, #[case] peer: &str) {
        assert!(trusted(&[entry]).trusts(peer.parse().expect("peer")));
    }

    #[test]
    fn a_bare_ip_trusts_only_itself() {
        assert!(!trusted(&["192.168.1.10"]).trusts("192.168.1.11".parse().expect("peer")));
    }

    #[test]
    fn rejects_an_invalid_entry() {
        let error =
            TrustedProxies::try_from(vec!["10.0.0.0/33".to_string()]).expect_err("invalid range");

        assert_eq!(
            error.to_string(),
            "`10.0.0.0/33` is not an IP address or CIDR range"
        );
    }

    #[test]
    fn deserializes_from_a_list_of_strings() {
        let proxies: TrustedProxies =
            serde_json::from_str(r#"["10.0.0.0/8", "192.168.1.10"]"#).expect("valid JSON");

        assert!(proxies.trusts("192.168.1.10".parse().expect("peer")));
    }

    mod warn_if_misconfigured {
        use tracing_test::traced_test;

        use super::*;

        #[test]
        #[traced_test]
        fn warns_about_a_catch_all_range() {
            trusted(&["0.0.0.0/0"]).warn_if_misconfigured(true);

            assert!(logs_contain("includes 0.0.0.0/0, which trusts every peer"));
        }

        #[test]
        #[traced_test]
        fn warns_when_client_addresses_are_not_recorded() {
            trusted(&["10.0.0.0/8"]).warn_if_misconfigured(false);

            assert!(logs_contain("has no effect"));
        }

        #[test]
        #[traced_test]
        fn stays_quiet_when_configured_and_recorded() {
            trusted(&["10.0.0.0/8"]).warn_if_misconfigured(true);

            assert!(!logs_contain("trusted_proxies"));
        }

        #[test]
        #[traced_test]
        fn stays_quiet_when_empty() {
            trusted(&[]).warn_if_misconfigured(false);

            assert!(!logs_contain("trusted_proxies"));
        }
    }

    #[rstest::rstest]
    #[case::none(&["10.0.0.0/8"], None)]
    #[case::ipv4(&["10.0.0.0/8", "0.0.0.0/0"], Some("0.0.0.0/0"))]
    #[case::ipv6(&["::/0"], Some("::/0"))]
    fn finds_a_catch_all_range(#[case] entries: &[&str], #[case] expected: Option<&str>) {
        assert_eq!(
            trusted(entries)
                .catch_all()
                .map(|net| net.to_string())
                .as_deref(),
            expected
        );
    }
}
