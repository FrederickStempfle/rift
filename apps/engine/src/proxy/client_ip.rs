use std::net::IpAddr;

use hyper::HeaderMap;
use ipnet::IpNet;

pub fn extract_client_ip(
    remote_ip: IpAddr,
    headers: &HeaderMap,
    trusted_proxies: &[IpNet],
) -> IpAddr {
    if !is_trusted_proxy(remote_ip, trusted_proxies) {
        return remote_ip;
    }

    if let Some(cf_connecting_ip) = header_ip(headers, "cf-connecting-ip") {
        return cf_connecting_ip;
    }

    let mut chain = forwarded_for_chain(headers);
    if !chain.is_empty() {
        chain.push(remote_ip);
        for ip in chain.iter().rev() {
            if !is_trusted_proxy(*ip, trusted_proxies) {
                return *ip;
            }
        }
        return chain[0];
    }

    header_ip(headers, "x-real-ip").unwrap_or(remote_ip)
}

fn forwarded_for_chain(headers: &HeaderMap) -> Vec<IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .filter_map(|item| item.parse::<IpAddr>().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn header_ip(headers: &HeaderMap, name: &'static str) -> Option<IpAddr> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<IpAddr>().ok())
}

pub fn is_trusted_proxy(ip: IpAddr, trusted_proxies: &[IpNet]) -> bool {
    trusted_proxies.iter().any(|cidr| cidr.contains(&ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted_list() -> Vec<IpNet> {
        vec![
            "127.0.0.1/32".parse().unwrap(),
            "10.0.0.0/8".parse().unwrap(),
            "172.16.0.0/12".parse().unwrap(),
        ]
    }

    #[test]
    fn uses_remote_ip_when_not_trusted_proxy() {
        let headers = HeaderMap::new();
        let ip = extract_client_ip("198.51.100.7".parse().unwrap(), &headers, &trusted_list());
        assert_eq!(ip, "198.51.100.7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn prefers_cf_connecting_ip_for_trusted_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "203.0.113.42".parse().unwrap());
        headers.insert(
            "x-forwarded-for",
            "203.0.113.50, 10.1.1.5".parse().unwrap(),
        );
        let ip = extract_client_ip("10.9.0.2".parse().unwrap(), &headers, &trusted_list());
        assert_eq!(ip, "203.0.113.42".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn extracts_first_untrusted_from_forward_chain() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.9, 10.10.2.4, 172.16.10.4".parse().unwrap(),
        );
        let ip = extract_client_ip("10.9.0.2".parse().unwrap(), &headers, &trusted_list());
        assert_eq!(ip, "198.51.100.9".parse::<IpAddr>().unwrap());
    }
}
