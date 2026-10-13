//! Whether macOS marks the current path expensive (hotspot, cellular) or constrained (Low Data Mode).
//! Scheduled bandwidth tests skip such paths.

use block2::RcBlock;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    fn nw_path_monitor_create() -> *mut c_void;
    fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: *mut c_void);
    fn nw_path_monitor_set_update_handler(monitor: *mut c_void, handler: &block2::DynBlock<dyn Fn(*mut c_void)>);
    fn nw_path_monitor_start(monitor: *mut c_void);
    fn nw_path_is_expensive(path: *mut c_void) -> bool;
    fn nw_path_is_constrained(path: *mut c_void) -> bool;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
}

static EXPENSIVE: AtomicBool = AtomicBool::new(false);
static CONSTRAINED: AtomicBool = AtomicBool::new(false);
static SEEN: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();

/// Starts the one monitor for the process; it lives until exit.
pub fn start() {
    START.call_once(|| unsafe {
        let m = nw_path_monitor_create();
        if m.is_null() {
            return;
        }
        let q = dispatch_queue_create(c"packet.path".as_ptr(), std::ptr::null());
        nw_path_monitor_set_queue(m, q);
        let block = RcBlock::new(|path: *mut c_void| {
            if !path.is_null() {
                EXPENSIVE.store(nw_path_is_expensive(path), Ordering::Relaxed);
                CONSTRAINED.store(nw_path_is_constrained(path), Ordering::Relaxed);
                SEEN.store(true, Ordering::Release);
            }
        });
        nw_path_monitor_set_update_handler(m, &block);
        nw_path_monitor_start(m);
        std::mem::forget(block);
    });
}

/// (expensive, constrained). Waits briefly for the first update so a CLI run gets a real answer.
pub fn state() -> (bool, bool) {
    start();
    let t = Instant::now();
    while !SEEN.load(Ordering::Acquire) && t.elapsed() < Duration::from_millis(500) {
        std::thread::sleep(Duration::from_millis(10));
    }
    (EXPENSIVE.load(Ordering::Relaxed), CONSTRAINED.load(Ordering::Relaxed))
}
