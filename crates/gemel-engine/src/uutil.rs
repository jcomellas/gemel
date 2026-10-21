//! The progress and update-status printers a user interface installs.
//!
//! Ports the printers of Unison `src/uutil.ml` (`setProgressPrinter`,
//! `showProgress`, `setUpdateStatusPrinter` and `showUpdateStatus`).
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! OCaml keeps them in globals of the process; here they are a host's
//! [`Printers`] slot. Progress is reported on the client's host, where
//! copies between local roots run. A scan reports its status on the host
//! that scans, so a server's (one in loopback mode, as on a real one) has
//! none. Both are called off the engine thread too, from the blocking
//! tasks that copy and scan, so they are `Send` and `Sync`.

use std::sync::Arc;

use gemel_base::util::FileId;
use gemel_fs::props::Filesize;
use gemel_rpc::host::Host;

/// Gets each count of bytes an item has transferred, and a tag for where
/// it was counted (`"l"` for a local copy, `"mov"` for a move).
pub type ProgressPrinter = Arc<dyn Fn(FileId, Filesize, &'static str) + Send + Sync>;

/// Gets the path being scanned, now and then; or `""` when the scan is
/// done.
pub type UpdateStatusPrinter = Arc<dyn Fn(&str) + Send + Sync>;

/// A host's printers; none at first, which shows nothing.
#[derive(Clone, Default)]
pub struct Printers {
    /// `Uutil.progressPrinter`.
    pub progress: Option<ProgressPrinter>,
    /// `Uutil.statusPrinter`.
    pub update_status: Option<UpdateStatusPrinter>,
}

/// `Uutil.setProgressPrinter`, on `host`; `None` shows nothing.
pub fn set_progress_printer(host: &Host, p: Option<ProgressPrinter>) {
    host.state::<Printers>().borrow_mut().progress = p;
}

/// `Uutil.setUpdateStatusPrinter`, on `host`.
pub fn set_update_status_printer(host: &Host, p: Option<UpdateStatusPrinter>) {
    host.state::<Printers>().borrow_mut().update_status = p;
}

/// `host`'s progress printer, for a task off the engine thread.
pub fn progress_printer(host: &Host) -> Option<ProgressPrinter> {
    host.state::<Printers>().borrow().progress.clone()
}

/// `host`'s update-status printer, for a scan off the engine thread.
pub fn update_status_printer(host: &Host) -> Option<UpdateStatusPrinter> {
    host.state::<Printers>().borrow().update_status.clone()
}

/// `Uutil.showProgress`: reports `bytes` more transferred for item `id`,
/// unless it is the dummy.
pub fn show_progress(host: &Host, id: FileId, bytes: Filesize, tag: &'static str) {
    if id == FileId::DUMMY {
        return;
    }
    if let Some(p) = progress_printer(host) {
        p(id, bytes, tag);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn progress_goes_to_the_printer_except_for_the_dummy() {
        let host = Host::client(Default::default(), Default::default());
        // Nothing installed: nothing happens.
        show_progress(&host, FileId(1), 10, "l");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        set_progress_printer(
            &host,
            Some(Arc::new(move |id, n, tag| {
                s.lock().unwrap().push((id, n, tag))
            })),
        );
        show_progress(&host, FileId(1), 10, "l");
        show_progress(&host, FileId::DUMMY, 20, "l");
        // A printer taken for another thread is the same.
        let p = progress_printer(&host).unwrap();
        std::thread::spawn(move || p(FileId(2), 30, "mov"))
            .join()
            .unwrap();
        set_progress_printer(&host, None);
        show_progress(&host, FileId(1), 40, "l");
        assert_eq!(
            *seen.lock().unwrap(),
            [(FileId(1), 10, "l"), (FileId(2), 30, "mov")]
        );
    }
}
