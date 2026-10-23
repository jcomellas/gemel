//! Looking up the addresses a socket connects to or listens on
//! (`Unix.getaddrinfo`).
//!
//! Ports what Unison `src/remote.ml` uses of OCaml's `Unix.getaddrinfo`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! As OCaml calls it, with `AI_SOCKTYPE SOCK_STREAM` (and `AI_PASSIVE` for
//! a server): an empty host or service is passed as none, the addresses
//! come in the resolver's order, and a failure gives no address rather
//! than an error.

use std::net::SocketAddr;

/// `Unix.getaddrinfo node service [AI_SOCKTYPE SOCK_STREAM]`, with
/// `AI_PASSIVE` if `passive`: the stream addresses for `node` (a host
/// name or address, or none if empty) and `service` (a port number or
/// service name, or none if empty). Only IPv4 and IPv6 addresses.
pub fn getaddrinfo(node: &str, service: &str, passive: bool) -> Vec<SocketAddr> {
    imp::getaddrinfo(node, service, passive)
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::CString;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
    use std::ptr;

    pub fn getaddrinfo(node: &str, service: &str, passive: bool) -> Vec<SocketAddr> {
        let c = |s: &str| (!s.is_empty()).then(|| CString::new(s)).transpose();
        let (Ok(node), Ok(service)) = (c(node), c(service)) else {
            return Vec::new();
        };
        let as_ptr = |s: &Option<CString>| s.as_ref().map_or(ptr::null(), |s| s.as_ptr());
        // SAFETY: all-zero is a valid `addrinfo` (null pointers, no flags).
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = libc::AF_UNSPEC;
        hints.ai_socktype = libc::SOCK_STREAM;
        if passive {
            hints.ai_flags = libc::AI_PASSIVE;
        }
        let mut res: *mut libc::addrinfo = ptr::null_mut();
        // SAFETY: the strings are NUL-terminated or null, `hints` is
        // initialised, and `res` receives a list freed below.
        let r = unsafe { libc::getaddrinfo(as_ptr(&node), as_ptr(&service), &hints, &mut res) };
        if r != 0 {
            return Vec::new();
        }
        let mut addrs = Vec::new();
        let mut ai = res;
        while !ai.is_null() {
            // SAFETY: `ai` is a node of the list getaddrinfo returned, and
            // its address is as long as its family says.
            unsafe {
                let a = &*ai;
                match a.ai_family {
                    libc::AF_INET if !a.ai_addr.is_null() => {
                        let sin = &*(a.ai_addr as *const libc::sockaddr_in);
                        addrs.push(SocketAddr::V4(SocketAddrV4::new(
                            Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)),
                            u16::from_be(sin.sin_port),
                        )));
                    }
                    libc::AF_INET6 if !a.ai_addr.is_null() => {
                        let sin6 = &*(a.ai_addr as *const libc::sockaddr_in6);
                        addrs.push(SocketAddr::V6(SocketAddrV6::new(
                            Ipv6Addr::from(sin6.sin6_addr.s6_addr),
                            u16::from_be(sin6.sin6_port),
                            sin6.sin6_flowinfo,
                            sin6.sin6_scope_id,
                        )));
                    }
                    _ => {}
                }
                ai = a.ai_next;
            }
        }
        // SAFETY: `res` is the list getaddrinfo returned, freed once.
        unsafe { libc::freeaddrinfo(res) };
        addrs
    }
}

/// Windows: through the standard library, which takes numeric ports only.
/// Sockets on Windows are phase 6 of milestone 3.
#[cfg(not(unix))]
mod imp {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

    pub fn getaddrinfo(node: &str, service: &str, passive: bool) -> Vec<SocketAddr> {
        let port = if service.is_empty() {
            Ok(0)
        } else {
            service.parse::<u16>()
        };
        let Ok(port) = port else {
            return Vec::new();
        };
        if node.is_empty() {
            let (v6, v4) = if passive {
                (Ipv6Addr::UNSPECIFIED, Ipv4Addr::UNSPECIFIED)
            } else {
                (Ipv6Addr::LOCALHOST, Ipv4Addr::LOCALHOST)
            };
            return vec![(v6, port).into(), (v4, port).into()];
        }
        (node, port)
            .to_socket_addrs()
            .map(Iterator::collect)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_hosts_and_ports_are_looked_up() {
        assert_eq!(
            getaddrinfo("127.0.0.1", "5000", false),
            ["127.0.0.1:5000".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(
            getaddrinfo("::1", "80", false),
            ["[::1]:80".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn a_server_with_no_host_listens_on_every_address() {
        let addrs = getaddrinfo("", "5000", true);
        assert!(!addrs.is_empty());
        assert!(
            addrs
                .iter()
                .all(|a| a.ip().is_unspecified() && a.port() == 5000)
        );
    }

    #[test]
    fn what_doesnt_resolve_gives_no_address() {
        assert!(getaddrinfo("no-such-host.invalid", "5000", false).is_empty());
        assert!(getaddrinfo("127.0.0.1", "no-such-service", false).is_empty());
    }
}
