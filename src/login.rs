//! Open-at-login through SMAppService (macOS 13+). Only works from the .app bundle.

use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginState {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
    Unavailable,
}

/// SMAppServiceStatus raw values.
pub fn state_from(raw: isize) -> LoginState {
    match raw {
        0 => LoginState::NotRegistered,
        1 => LoginState::Enabled,
        2 => LoginState::RequiresApproval,
        3 => LoginState::NotFound,
        _ => LoginState::Unavailable,
    }
}

impl LoginState {
    /// Menu label for the open-at-login item.
    pub fn label(self) -> &'static str {
        match self {
            LoginState::RequiresApproval => "Open at login — approve in Login Items…",
            LoginState::NotFound | LoginState::Unavailable => "Open at login (run from /Applications)",
            _ => "Open at login",
        }
    }
}

fn service() -> Option<*mut AnyObject> {
    let cls = AnyClass::get(c"SMAppService")?;
    let svc: *mut AnyObject = unsafe { msg_send![cls, mainAppService] };
    (!svc.is_null()).then_some(svc)
}

pub fn state() -> LoginState {
    match service() {
        Some(s) => state_from(unsafe { msg_send![s, status] }),
        None => LoginState::Unavailable,
    }
}

/// Registers or unregisters; on failure returns the NSError code.
pub fn set(on: bool) -> Result<(), isize> {
    let Some(s) = service() else { return Err(-1) };
    let mut err: *mut AnyObject = std::ptr::null_mut();
    let ok: bool = unsafe {
        if on {
            msg_send![s, registerAndReturnError: &mut err]
        } else {
            msg_send![s, unregisterAndReturnError: &mut err]
        }
    };
    if ok {
        Ok(())
    } else if err.is_null() {
        Err(-1)
    } else {
        Err(unsafe { msg_send![err, code] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_states() {
        assert_eq!(state_from(1), LoginState::Enabled);
        assert_eq!(state_from(2), LoginState::RequiresApproval);
        assert_eq!(state_from(0), LoginState::NotRegistered);
        assert_eq!(state_from(9), LoginState::Unavailable);
        assert!(LoginState::RequiresApproval.label().contains("approve"));
    }
}
