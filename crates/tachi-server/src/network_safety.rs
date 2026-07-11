use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Blocks anything that is not plausibly a global-unicast, internet-routable
/// address: loopback, private, link-local, multicast, and the IANA
/// special-purpose registry entries relevant to SSRF.
///
/// Deliberately NOT built on the unstable `Ipv4Addr::is_global()` /
/// `Ipv6Addr::is_global()` family (still nightly-gated behind the `ip`
/// feature on this toolchain) — instead this is an explicit, documented table
/// so every blocked range is visible and independently testable, per range,
/// rather than approximated by std's `is_private()`/`is_loopback()` alone.
/// Several SSRF-relevant ranges are NOT covered by those stable-only helpers:
/// - `0.0.0.0/8` ("this network", RFC 791 §3.2) — `is_unspecified()` only
///   matches the single address `0.0.0.0`, not the whole `/8`. This matters
///   in practice: a bare numeral host like `http://127/` is normalized by
///   the URL parser to `0.0.0.127`, which slips past `is_loopback()`.
/// - `100.64.0.0/10` (Carrier-Grade NAT / Shared Address Space, RFC 6598).
/// - `198.18.0.0/15` (benchmarking, RFC 2544) — the exact fake-IP range
///   implicated in tachi#926.
/// - `240.0.0.0/4` (reserved / Class E, RFC 1112 §4).
/// - `2001:db8::/32` (IPv6 documentation, RFC 3849) — IPv4 has
///   `is_documentation()`; IPv6 has no stable equivalent.
pub(crate) fn is_private_or_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

fn is_blocked_ipv4(v4: Ipv4Addr) -> bool {
    if v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_documentation()
        || v4.is_multicast()
    {
        return true;
    }
    let o = v4.octets();
    // 0.0.0.0/8 — "this network" (RFC 791 §3.2).
    if o[0] == 0 {
        return true;
    }
    // 100.64.0.0/10 — Carrier-Grade NAT / Shared Address Space (RFC 6598).
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return true;
    }
    // 198.18.0.0/15 — benchmarking (RFC 2544); tachi#926's fake-IP range.
    if o[0] == 198 && (18..=19).contains(&o[1]) {
        return true;
    }
    // 240.0.0.0/4 — reserved / Class E (RFC 1112 §4). Also catches
    // 255.255.255.255, already covered by is_broadcast() above.
    if o[0] >= 240 {
        return true;
    }
    false
}

fn is_blocked_ipv6(v6: Ipv6Addr) -> bool {
    if v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || v6.is_unique_local()
        || v6.is_unicast_link_local()
    {
        return true;
    }
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_blocked_ipv4(v4);
    }
    let seg = v6.segments();
    // 2001:db8::/32 — documentation (RFC 3849); no stable is_documentation()
    // for IPv6, unlike Ipv4Addr.
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_private_or_local_ip;
    use std::net::IpAddr;

    fn assert_blocked(raw: &str) {
        let ip = raw
            .parse::<IpAddr>()
            .unwrap_or_else(|e| panic!("parse {raw}: {e}"));
        assert!(is_private_or_local_ip(ip), "{raw} should be blocked");
    }

    fn assert_allowed(raw: &str) {
        let ip = raw
            .parse::<IpAddr>()
            .unwrap_or_else(|e| panic!("parse {raw}: {e}"));
        assert!(!is_private_or_local_ip(ip), "{raw} should be allowed");
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6_private_ranges() {
        for raw in ["::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:192.168.1.1"] {
            assert_blocked(raw);
        }
    }

    #[test]
    fn allows_public_ips() {
        for raw in ["93.184.216.34", "2606:2800:220:1:248:1893:25c8:1946"] {
            assert_allowed(raw);
        }
    }

    #[test]
    fn blocks_this_network_slash_8() {
        // 0.0.0.0/8 — including non-.0 hosts, which is what a bare numeral
        // like `http://127/` normalizes to (0.0.0.127), previously unblocked.
        for raw in ["0.0.0.0", "0.0.0.1", "0.0.0.127", "0.255.255.255"] {
            assert_blocked(raw);
        }
    }

    #[test]
    fn blocks_cgnat_shared_address_space() {
        // 100.64.0.0/10
        for raw in ["100.64.0.0", "100.64.0.1", "100.100.0.1", "100.127.255.255"] {
            assert_blocked(raw);
        }
        // Just outside the /10 on both sides must stay allowed.
        assert_allowed("100.63.255.255");
        assert_allowed("100.128.0.0");
    }

    #[test]
    fn blocks_benchmarking_range() {
        // 198.18.0.0/15 — tachi#926's fake-IP range.
        for raw in ["198.18.0.0", "198.18.0.1", "198.19.255.255"] {
            assert_blocked(raw);
        }
        assert_allowed("198.17.255.255");
        assert_allowed("198.20.0.0");
    }

    #[test]
    fn blocks_reserved_class_e() {
        // 240.0.0.0/4
        for raw in ["240.0.0.0", "250.1.2.3", "255.255.255.254"] {
            assert_blocked(raw);
        }
        // 224.0.0.0/4 through 239.255.255.255 is multicast (already blocked
        // by is_multicast()); the genuine "just below the reserved range"
        // boundary probe is 223.255.255.255, the last non-multicast address.
        assert_allowed("223.255.255.255");
    }

    #[test]
    fn blocks_ipv6_documentation_range() {
        for raw in ["2001:db8::1", "2001:db8:1234::5678"] {
            assert_blocked(raw);
        }
        assert_allowed("2001:db9::1");
    }
}
