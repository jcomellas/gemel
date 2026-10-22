//! Canonical roots (`Common.root`) and resolving the roots users give.
//!
//! Ports parts of Unison `src/common.ml` and `src/remote.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

use std::cmp::Ordering;
use std::fmt;

use gemel_base::clroot::Clroot;
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader};
use gemel_fs::fspath::Fspath;

use crate::registry::{HostCmd, Rpc};

/// Where a root is (`Common.host`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    /// This machine.
    Local,
    /// A server, by its canonical host name.
    Remote(String),
}

/// A canonical root: a host and an absolute path on it (`Common.root`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Root {
    /// The host.
    pub host: Host,
    /// The path on that host.
    pub fspath: Fspath,
}

impl Root {
    /// A local root.
    pub fn local(fspath: Fspath) -> Root {
        Root {
            host: Host::Local,
            fspath,
        }
    }

    /// `Common.root2hostname`.
    pub fn hostname(&self) -> &str {
        match &self.host {
            Host::Local => "local",
            Host::Remote(h) => h,
        }
    }

    /// Whether the root is on this machine.
    pub fn is_local(&self) -> bool {
        self.host == Host::Local
    }
}

/// `Common.root2string`: the path, prefixed with `//host/` if remote.
impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            Host::Local => write!(f, "{}", self.fspath),
            Host::Remote(h) => write!(f, "//{h}/{}", self.fspath),
        }
    }
}

/// `Common.compareRoots`: local roots first, then by host, then by path
/// (bytes, whatever the case mode).
impl Ord for Root {
    fn cmp(&self, other: &Self) -> Ordering {
        match (&self.host, &other.host) {
            (Host::Local, Host::Local) => self.fspath.cmp(&other.fspath),
            (Host::Local, Host::Remote(_)) => Ordering::Less,
            (Host::Remote(_), Host::Local) => Ordering::Greater,
            (Host::Remote(a), Host::Remote(b)) => {
                a.cmp(b).then_with(|| self.fspath.cmp(&other.fspath))
            }
        }
    }
}

impl PartialOrd for Root {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `Common.sortRoots`: the canonical order, in which commands run on roots.
pub fn sort_roots(roots: &mut [Root]) {
    roots.sort();
}

/// `canonizeOnServer`'s argument: the path, as given, and a flag.
pub type CanonizeArgs = (Option<Vec<u8>>, bool);

/// `canonizeOnServer`'s result: the host name and the canonical path.
pub type CanonizeResult = (String, Fspath);

/// `canonizeOnServer`: the server's host name and the canonical form of a
/// path on it. (The boolean is ignored; it was needed by Unison ≤ 2.52.)
pub static CANONIZE_ON_SERVER: HostCmd<CanonizeArgs, CanonizeResult> = HostCmd::new(
    "canonizeOnServer",
    |(s, b), w, _| {
        w.option(s.as_ref(), |w, s| w.string(s));
        w.bool(*b);
    },
    |r, _| Ok((r.option(Reader::string)?, r.bool()?)),
    |(h, f), w, _| {
        w.string(h.as_bytes());
        f.marshal(w);
    },
    |r, _| Ok((read_text(r)?, Fspath::unmarshal(r)?)),
    |host, (s, _)| {
        Box::pin(async move {
            let name = host.canonical_host_name();
            Ok((name, Fspath::canonize(s.as_deref())?))
        })
    },
);

fn read_text(r: &mut Reader<'_>) -> umarshal::Result<String> {
    Ok(String::from_utf8_lossy(&r.string()?).into_owned())
}

/// `Remote.canonizeRoot`: the canonical root for a root specification.
///
/// A local root is canonicalised through `canonizeOnServer`, so that in
/// loopback mode the command and its codecs are exercised as they will be
/// with a server; directly, it is `Fspath.canonize`, as in OCaml.
///
/// A remote root's server canonicalises its path and names itself
/// (`canonizeOnServer`), over the connection the client already has to it
/// (`ClientConn.ofClrootConncheck`), which is then registered under the
/// canonical root. Making the connection comes later in milestone 3.
pub async fn canonize_root(rpc: &Rpc, clroot: &Clroot) -> Result<Root, Error> {
    let path = match clroot {
        Clroot::ConnectLocal(s) => {
            let arg = (s.as_ref().map(|s| s.as_bytes().to_vec()), true);
            let (_, fspath) = rpc
                .call_host(&CANONIZE_ON_SERVER, &Host::Local, arg)
                .await?;
            return Ok(Root::local(fspath));
        }
        Clroot::ConnectByShell { path, .. } | Clroot::ConnectBySocket { path, .. } => path,
    };
    let Some(conn) = rpc.connection_checked(clroot).await else {
        return Err(Error::Fatal(format!(
            "Remote roots are not supported yet: {clroot}"
        )));
    };
    let arg = (path.as_ref().map(|s| s.as_bytes().to_vec()), true);
    let (host, fspath) = conn.call_host(&CANONIZE_ON_SERVER, arg).await?;
    let root = Root {
        host: Host::Remote(host),
        fspath,
    };
    rpc.name_connection(clroot.clone(), root.clone(), conn);
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(host: Option<&str>, p: &str) -> Root {
        Root {
            host: host.map_or(Host::Local, |h| Host::Remote(h.into())),
            fspath: Fspath::from_canonical(p.as_bytes().to_vec()),
        }
    }

    #[test]
    fn order_and_printing() {
        let mut roots = vec![
            root(Some("b"), "/x"),
            root(None, "/z"),
            root(Some("a"), "/y"),
            root(None, "/a"),
        ];
        sort_roots(&mut roots);
        let shown: Vec<String> = roots.iter().map(ToString::to_string).collect();
        assert_eq!(shown, ["/a", "/z", "//a//y", "//b//x"]);
        assert_eq!(roots[0].hostname(), "local");
        assert_eq!(roots[2].hostname(), "a");
    }
}
