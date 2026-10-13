//! Wi-Fi link quality through CoreWLAN. Reads signal figures only; the network name needs
//! Location permission and is never read.

use crate::probe::WiFi;
use objc2::msg_send;
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, AnyObject};
use std::ffi::{c_char, CStr};

#[link(name = "CoreWLAN", kind = "framework")]
unsafe extern "C" {}

/// The Wi-Fi interface name (even when idle) and, when associated, its signal figures.
pub fn current() -> (Option<String>, Option<WiFi>) {
    autoreleasepool(|_| unsafe {
        let Some(cls) = AnyClass::get(c"CWWiFiClient") else { return (None, None) };
        let client: *mut AnyObject = msg_send![cls, sharedWiFiClient];
        if client.is_null() {
            return (None, None);
        }
        let iface: *mut AnyObject = msg_send![client, interface];
        if iface.is_null() {
            return (None, None);
        }
        let name: *mut AnyObject = msg_send![iface, interfaceName];
        let name = if name.is_null() {
            None
        } else {
            let p: *const c_char = msg_send![name, UTF8String];
            (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
        };
        let rssi: isize = msg_send![iface, rssiValue];
        if rssi == 0 {
            return (name, None);
        }
        let noise: isize = msg_send![iface, noiseMeasurement];
        let tx: f64 = msg_send![iface, transmitRate];
        let ch: *mut AnyObject = msg_send![iface, wlanChannel];
        let (channel, band) = if ch.is_null() {
            (0, 0)
        } else {
            let n: isize = msg_send![ch, channelNumber];
            let b: isize = msg_send![ch, channelBand];
            (n as i64, b as i64)
        };
        (name, Some(WiFi { rssi: rssi as i64, noise: noise as i64, tx_mbps: tx, channel, band }))
    })
}
