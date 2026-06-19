use std::net::IpAddr;

pub(crate) fn is_private_or_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_private_or_local_ip(IpAddr::V4(v4)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_private_or_local_ip;
    use std::net::IpAddr;

    #[test]
    fn blocks_ipv4_mapped_ipv6_private_ranges() {
        for raw in ["::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:192.168.1.1"] {
            let ip = raw.parse::<IpAddr>().unwrap();
            assert!(is_private_or_local_ip(ip), "{raw} should be blocked");
        }
    }

    #[test]
    fn allows_public_ips() {
        for raw in ["93.184.216.34", "2606:2800:220:1:248:1893:25c8:1946"] {
            let ip = raw.parse::<IpAddr>().unwrap();
            assert!(!is_private_or_local_ip(ip), "{raw} should be allowed");
        }
    }
}
