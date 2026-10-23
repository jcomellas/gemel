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

/// Windows: Winsock's, as OCaml's calls it there.
#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::CString;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
    use std::ptr;
    use std::sync::Once;

    use windows_sys::Win32::Networking::WinSock::{
        ADDRINFOA, AF_INET, AF_INET6, AF_UNSPEC, AI_PASSIVE, SOCK_STREAM, SOCKADDR_IN,
        SOCKADDR_IN6, WSADATA, WSAStartup, freeaddrinfo, getaddrinfo as winsock_getaddrinfo,
    };

    /// Winsock is started once, before its first use (OCaml's `Unix`
    /// starts it as it loads); it stays started.
    fn start_winsock() {
        static START: Once = Once::new();
        START.call_once(|| {
            // SAFETY: all-zero is a valid `WSADATA`, which receives the
            // details.
            let mut data: WSADATA = unsafe { std::mem::zeroed() };
            // SAFETY: `data` is writable.
            unsafe { WSAStartup(0x0202, &mut data) };
        });
    }

    pub fn getaddrinfo(node: &str, service: &str, passive: bool) -> Vec<SocketAddr> {
        let c = |s: &str| (!s.is_empty()).then(|| CString::new(s)).transpose();
        let (Ok(node), Ok(service)) = (c(node), c(service)) else {
            return Vec::new();
        };
        start_winsock();
        let as_ptr = |s: &Option<CString>| s.as_ref().map_or(ptr::null(), |s| s.as_ptr().cast());
        let hints = ADDRINFOA {
            ai_family: i32::from(AF_UNSPEC),
            ai_socktype: SOCK_STREAM,
            ai_flags: if passive { AI_PASSIVE as i32 } else { 0 },
            ..ADDRINFOA::default()
        };
        let mut res: *mut ADDRINFOA = ptr::null_mut();
        // SAFETY: the strings are NUL-terminated or null, `hints` is
        // initialised, and `res` receives a list freed below.
        let r = unsafe { winsock_getaddrinfo(as_ptr(&node), as_ptr(&service), &hints, &mut res) };
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
                if a.ai_addr.is_null() {
                } else if a.ai_family == i32::from(AF_INET) {
                    let sin = &*(a.ai_addr as *const SOCKADDR_IN);
                    addrs.push(SocketAddr::V4(SocketAddrV4::new(
                        Ipv4Addr::from(u32::from_be(sin.sin_addr.S_un.S_addr)),
                        u16::from_be(sin.sin_port),
                    )));
                } else if a.ai_family == i32::from(AF_INET6) {
                    let sin6 = &*(a.ai_addr as *const SOCKADDR_IN6);
                    addrs.push(SocketAddr::V6(SocketAddrV6::new(
                        Ipv6Addr::from(sin6.sin6_addr.u.Byte),
                        u16::from_be(sin6.sin6_port),
                        sin6.sin6_flowinfo,
                        sin6.Anonymous.sin6_scope_id,
                    )));
                }
                ai = a.ai_next;
            }
        }
        // SAFETY: `res` is the list getaddrinfo returned, freed once.
        unsafe { freeaddrinfo(res) };
        addrs
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

    /// Services by name, as the system lists them (on Windows too).
    #[test]
    fn service_names_are_looked_up() {
        assert_eq!(
            getaddrinfo("127.0.0.1", "http", false),
            ["127.0.0.1:80".parse::<SocketAddr>().unwrap()]
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
