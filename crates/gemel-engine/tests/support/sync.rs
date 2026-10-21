//! A client with two local roots, and the trees and checks the scan and
//! commit tests share.

use std::cell::RefCell;
use std::path::{Path as StdPath, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gemel_base::case::Mode;
use gemel_base::path::{Global, Path};
use gemel_base::prefs::Prefs;
use gemel_base::trace::Trace;
use gemel_engine::archive::{self, Archive};
use gemel_engine::common::{UpdateContent, UpdateItem};
use gemel_engine::globals::{self, Roots};
use gemel_engine::tree::Tree;
use gemel_engine::update::{self, PathUpdates};
use gemel_rpc::host::Host;
use gemel_rpc::registry::{Dispatch, Rpc};

use super::*;

pub fn global(s: &str) -> Path<Global> {
    Path::from_bytes(s.as_bytes()).unwrap()
}

pub const T0: f64 = 1_600_000_000.0;

/// A client with two local roots `a` and `b` in `dir`, archives in `dir/u`.
pub struct Setup {
    pub rpc: Rpc,
    pub roots: Roots,
    pub base: PathBuf,
    /// What the client's trace printed (log entries and messages).
    pub output: Output,
}

/// Captured output.
#[derive(Clone, Default)]
pub struct Output(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Output {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Output {
    /// Everything so far, and clears it.
    pub fn take(&self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut *self.0.lock().unwrap())).into_owned()
    }
}

pub fn setup(dir: &TempDir, loopback: bool) -> Setup {
    setup_in(dir, loopback, Mode::Sensitive)
}

/// [`setup`] in a case mode. (Roots are canonized when installed, so the
/// directory needn't be.)
pub fn setup_in(dir: &TempDir, loopback: bool, mode: Mode) -> Setup {
    let base = dir.0.clone();
    let raw = ["a", "b"].map(|n| {
        let p = base.join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_str().unwrap().to_owned()
    });
    let unison_dir = base.join("u");
    std::fs::create_dir_all(&unison_dir).unwrap();
    let mut prefs = new_prefs();
    globals::set_raw_roots(&mut prefs, &raw);
    prefs.set(&globals::PATHS, vec![Path::empty()]);
    prefs.set(&gemel_fs::props::TIMES, true);
    settings(mode).store(&mut prefs);
    let client = Rc::new(Host::client(
        Rc::new(RefCell::new(prefs)),
        Rc::new(RefCell::new(new_features())),
    ));
    client.set_unison_dir(&unison_dir);
    client.set_warn_printer(None);
    // The log file is `dir/u/unison.log`.
    let output = Output::default();
    let mut trace = Trace::with_sinks(Box::new(output.clone()), Box::new(output.clone()));
    trace.set_unison_dir(&unison_dir);
    *client.state::<Trace>().borrow_mut() = trace;
    let dispatch = if loopback {
        let server = Host::loopback_server(
            &client.prefs().borrow(),
            &client.features().borrow(),
            new_prefs(),
            new_features(),
        )
        .unwrap();
        server.set_unison_dir(&unison_dir);
        Dispatch::Loopback {
            client,
            server: Rc::new(server),
        }
    } else {
        Dispatch::Direct(client)
    };
    let mut rpc = Rpc::new(dispatch);
    gemel_engine::register_commands(&mut rpc).unwrap();
    let roots = block_on(async {
        let raw = globals::raw_roots(&rpc.client_host().prefs().borrow());
        let roots = Roots::install(&rpc, &raw).await.unwrap();
        archive::store_roots_name(&rpc, &roots).unwrap();
        globals::propagate_prefs(&rpc, &roots).await.unwrap();
        roots
    });
    Setup {
        rpc,
        roots,
        base,
        output,
    }
}

impl Setup {
    pub fn host(&self) -> &Host {
        self.rpc.local_host()
    }

    pub fn root(&self, side: &str) -> PathBuf {
        self.base.join(side)
    }

    pub fn a(&self) -> PathBuf {
        self.root("a")
    }

    pub fn b(&self) -> PathBuf {
        self.root("b")
    }

    pub fn this_root(&self, i: usize) -> String {
        archive::this_roots_global_name(self.host(), &self.roots.list()[i].fspath).unwrap()
    }

    pub fn find(&self) -> Vec<PathUpdates> {
        block_on(update::find_updates(&self.rpc, &self.roots, false, None)).unwrap()
    }

    /// `Stasher.initBackups`, as OCaml's startup runs it: on each root's
    /// host, so twice on a host with both (which ignores old backup files
    /// twice).
    pub fn init_backups(&self) {
        block_on(gemel_engine::stasher::init_backups(&self.rpc, &self.roots)).unwrap();
    }

    /// Changes the client's preferences and sends them to the hosts.
    pub fn set_prefs(&self, f: impl FnOnce(&mut Prefs)) {
        f(&mut self.rpc.client_host().prefs().borrow_mut());
        block_on(globals::propagate_prefs(&self.rpc, &self.roots)).unwrap();
    }

    /// Commits replicas found the same on both roots (`markEqual` at the
    /// root, then `commitUpdates`), as a sync of identical trees does.
    pub fn commit(&self, found: &[PathUpdates]) {
        let [(fa, fb)] = found else {
            panic!("one path")
        };
        let uc = |ui: &UpdateItem| match ui {
            UpdateItem::Updates(uc, _) => uc.clone(),
            other => panic!("{other:?}"),
        };
        let tree = Tree::Leaf((uc(&fa.1), uc(&fb.1)));
        block_on(async {
            update::mark_equal(&self.rpc, &self.roots, &tree)
                .await
                .unwrap();
            update::commit_updates(&self.rpc, &self.roots)
                .await
                .unwrap();
        });
    }

    /// The archive at `path` in root `i`, as cached on the host.
    pub fn archived(&self, i: usize, path: &str) -> Archive {
        archive::with_archive(self.host(), &self.this_root(i), |a| {
            update::get_path_in_archive(a, &global(path)).1.clone()
        })
        .unwrap()
    }
}

pub fn children(ui: &UpdateItem) -> Vec<(String, &UpdateItem)> {
    match ui {
        UpdateItem::Updates(UpdateContent::Dir(_, ch, _, _), _) => ch
            .iter()
            .map(|(n, ui)| (String::from_utf8_lossy(n.as_bytes()).into_owned(), ui))
            .collect(),
        other => panic!("not a directory: {other:?}"),
    }
}

pub fn child<'a>(ui: &'a UpdateItem, name: &str) -> &'a UpdateItem {
    children(ui)
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no update for {name} in {ui:?}"))
        .1
}

pub fn names(ui: &UpdateItem) -> Vec<String> {
    children(ui).into_iter().map(|(n, _)| n).collect()
}

pub fn md5(s: &str) -> Vec<u8> {
    <md5::Md5 as md5::Digest>::digest(s.as_bytes()).to_vec()
}

/// The replica: files, an empty directory and (on Unix) a symbolic link,
/// all with times in the past; directories' times are set last.
pub fn build_tree(root: &StdPath) {
    write(root, "README", "read me", T0);
    write(root, "src/main.c", "int main(void) { return 0; }", T0 + 1.0);
    write(root, "src/deep/x", "x", T0 + 2.0);
    std::fs::create_dir_all(root.join("empty")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("README", root.join("link")).unwrap();
    for d in ["src/deep", "src", "empty", ""] {
        set_mtime(&root.join(d), T0 + 10.0);
    }
}
