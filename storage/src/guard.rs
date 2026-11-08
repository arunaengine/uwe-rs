//! Keeps the requests of a confined run to hosts on public networks: names resolve only to
//! public addresses, and every redirect and every request through a proxy is checked first.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// How many redirects a request may follow.
const REDIRECTS: usize = 10;

/// The hosts that may be reached on any network; all others only on public networks.
#[derive(Clone, Debug, Default)]
pub(crate) struct Guard {
    allowed: Arc<[String]>,
}

impl Guard {
    pub fn new(allowed_hosts: &[String]) -> Guard {
        let allowed = allowed_hosts.iter().map(|host| plain(host)).collect();
        Guard { allowed }
    }

    fn allows(&self, host: &str) -> bool {
        self.allowed.contains(&plain(host))
    }

    /// Refuses an address whose host is a private address, or a name with a private address.
    pub async fn admit(&self, url: &reqwest::Url) -> Result<(), String> {
        let Some((host, port)) = self.named(url)? else {
            return Ok(());
        };
        let found = (tokio::net::lookup_host((host.as_str(), port)).await)
            .map_err(|error| format!("cannot resolve `{host}`: {error}"))?;
        self.public_all(&host, found)
    }

    /// [`Guard::admit`] for a redirect, which runs outside of async code.
    fn admit_redirect(&self, url: &reqwest::Url) -> Result<(), String> {
        use std::net::ToSocketAddrs;
        let Some((host, port)) = self.named(url)? else {
            return Ok(());
        };
        let lookup = || (host.as_str(), port).to_socket_addrs();
        let flavor = tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor());
        let found = match flavor {
            Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(lookup),
            _ => lookup(),
        };
        let found = found.map_err(|error| format!("cannot resolve `{host}`: {error}"))?;
        self.public_all(&host, found)
    }

    /// The host name and port to resolve, or none for an allowed host or a public address.
    fn named(&self, url: &reqwest::Url) -> Result<Option<(String, u16)>, String> {
        let host = plain(url.host_str().unwrap_or_default());
        if self.allows(&host) {
            return Ok(None);
        }
        match host.parse::<IpAddr>() {
            Ok(ip) if !public(ip) => Err(private(&host)),
            Ok(_) => Ok(None),
            Err(_) => Ok(Some((host, url.port_or_known_default().unwrap_or(443)))),
        }
    }

    /// Refuses `host` when one of its addresses is not public, unless the host is allowed.
    pub fn public_all(
        &self,
        host: &str,
        found: impl Iterator<Item = SocketAddr>,
    ) -> Result<(), String> {
        if self.allows(host) {
            return Ok(());
        }
        for address in found {
            if !public(address.ip()) {
                return Err(private(host));
            }
        }
        Ok(())
    }

    /// The builder with these rules for names and redirects. Proxy hosts may be private.
    pub fn client(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        let mut allowed: Vec<String> = self.allowed.to_vec();
        let names = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];
        for name in names.into_iter().chain(["ALL_PROXY", "all_proxy"]) {
            let proxy = std::env::var(name).ok();
            let url = proxy.and_then(|proxy| reqwest::Url::parse(&proxy).ok());
            allowed.extend(url.and_then(|url| url.host_str().map(plain)));
        }
        let proxy = crate::web::config_proxy().and_then(|proxy| reqwest::Url::parse(proxy).ok());
        allowed.extend(proxy.and_then(|url| url.host_str().map(plain)));
        let resolver = Resolver {
            allowed: allowed.into(),
        };
        let rules = self.clone();
        let redirects = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= REDIRECTS {
                return attempt.error("too many redirects");
            }
            match rules.admit_redirect(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(message) => attempt.error(message),
            }
        });
        builder.dns_resolver(Arc::new(resolver)).redirect(redirects)
    }
}

/// A resolver that leaves out the addresses of private and local networks.
struct Resolver {
    allowed: Arc<[String]>,
}

impl Resolve for Resolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = plain(name.as_str());
        let any = self.allowed.contains(&host);
        Box::pin(async move {
            let found: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| any || public(address.ip()))
                .collect();
            if found.is_empty() {
                return Err(private(&host).into());
            }
            Ok(Box::new(found.into_iter()) as Addrs)
        })
    }
}

/// A host in lower case without the brackets of an IPv6 address.
fn plain(host: &str) -> String {
    (host.trim_start_matches('[').trim_end_matches(']')).to_ascii_lowercase()
}

fn private(host: &str) -> String {
    format!("`{host}` is in a private or local network, which this run may not reach")
}

/// Whether an address belongs to the public internet: not loopback, private, link-local,
/// shared (100.64.0.0/10), unique local, multicast, unspecified or reserved.
pub(crate) fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => match (ip.to_ipv4_mapped(), nat64(ip)) {
            (Some(v4), _) | (None, Some(v4)) => public_v4(v4),
            (None, None) => public_v6(ip),
        },
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || a >= 240
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first == 0x2001 && ip.segments()[1] == 0x0db8))
}

/// The IPv4 address in a NAT64 address (`64:ff9b::/96`).
fn nat64(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    (s[..6] == [0x64, 0xff9b, 0, 0, 0, 0]).then(|| {
        let [a, b] = s[6].to_be_bytes();
        let [c, d] = s[7].to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted(guard: &Guard, url: &str) -> Result<(), String> {
        let url = reqwest::Url::parse(url).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(guard.admit(&url))
    }

    #[test]
    fn refuses_private_addresses() {
        let guard = Guard::default();
        for url in [
            "http://127.0.0.1/a",
            "https://10.1.2.3/a",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/a",
            "http://[fd00::1]/a",
            "http://[::ffff:192.168.1.1]/a",
            "http://100.64.0.1/a",
            "http://localhost/a",
        ] {
            assert!(admitted(&guard, url).is_err(), "{url} is refused");
        }
        assert!(admitted(&guard, "https://93.184.216.34/a").is_ok());
    }

    #[test]
    fn allows_named_hosts() {
        let guard = Guard::new(&["LocalHost".to_owned(), "10.1.2.3".to_owned()]);
        assert!(admitted(&guard, "http://localhost:8080/a").is_ok());
        assert!(admitted(&guard, "https://10.1.2.3/a").is_ok());
        assert!(admitted(&guard, "https://10.1.2.4/a").is_err());
    }
}
