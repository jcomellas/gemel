//! macOS's own Quit goes through `safeExit`, as the window's does.
//!
//! AppKit quits a program with `terminate:`: the application menu's Quit,
//! which Slint always adds (with Cmd-Q), the Dock's, and logging out. With
//! no `applicationShouldTerminate:` on the application's delegate, which
//! winit doesn't implement, AppKit ends the program at once, even while it
//! syncs, without asking or unlocking the archives. Here the delegate
//! lets it end at once only when the window's Quit would; otherwise it
//! answers not yet, and the window's Quit runs instead, which asks. So
//! logging out is cancelled only while the program works.

// The Objective-C runtime, to add a method to winit's delegate class.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::time::Duration;

use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{class, ffi, msg_send, sel};

/// Whether the program may end at once, and what runs instead when not.
type Hooks = (Box<dyn Fn() -> bool>, Box<dyn Fn()>);

thread_local! {
    /// `on_terminate`'s, on the main thread.
    static HOOKS: RefCell<Option<Hooks>> = const { RefCell::new(None) };
}

/// `NSTerminateCancel`.
const TERMINATE_CANCEL: usize = 0;
/// `NSTerminateNow`.
const TERMINATE_NOW: usize = 1;

/// `applicationShouldTerminate:`: now, if the program may end; if not,
/// not yet, and the window's Quit runs once AppKit's call has returned.
extern "C-unwind" fn should_terminate(_: &AnyObject, _: Sel, _: *mut AnyObject) -> usize {
    let now = HOOKS.with(|h| h.borrow().as_ref().is_none_or(|(may_end, _)| may_end()));
    if now {
        return TERMINATE_NOW;
    }
    slint::Timer::single_shot(Duration::ZERO, || {
        HOOKS.with(|h| {
            if let Some((_, quit)) = &*h.borrow() {
                quit();
            }
        })
    });
    TERMINATE_CANCEL
}

/// When macOS asks the program to quit, lets AppKit end it at once if
/// `may_end`, and otherwise has `quit` run instead. On the main thread,
/// once the event loop is made (which sets the application's delegate):
/// whether it took.
pub fn on_terminate(may_end: impl Fn() -> bool + 'static, quit: impl Fn() + 'static) -> bool {
    HOOKS.with(|h| *h.borrow_mut() = Some((Box::new(may_end), Box::new(quit))));
    // SAFETY: on the main thread, as AppKit wants. The delegate, if any, is
    // a live object. `should_terminate` has the signature of
    // `applicationShouldTerminate:`, which the type encoding says: an
    // NSUInteger (Q) from self, the selector and the sender.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![app, delegate];
        let Some(delegate) = delegate.as_ref() else {
            return false;
        };
        let imp = std::mem::transmute::<
            extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize,
            Imp,
        >(should_terminate);
        ffi::class_addMethod(
            delegate.class() as *const _ as *mut _,
            sel!(applicationShouldTerminate:),
            imp,
            c"Q@:@".as_ptr(),
        )
        .as_bool()
    }
}
