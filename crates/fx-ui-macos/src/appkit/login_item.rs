//! Launch at Login through `SMAppService.mainAppService` (macOS 13+).
//!
//! `SMAppService` only works for a bundled `.app`; run unbundled (for
//! example with `cargo run`) [`status`] reports
//! [`LoginItemStatus::Unavailable`] and [`set_enabled`] returns an error
//! instead of calling into ServiceManagement. Nothing here panics.

use std::fmt;
use std::path::Path;

use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// The state of the app's login item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginItemStatus {
    /// Registered and allowed to launch at login.
    Enabled,
    /// Registered, but the user must approve it in System Settings › Login
    /// Items.
    RequiresApproval,
    /// Not registered.
    NotRegistered,
    /// ServiceManagement could not find the app (for example a copy that
    /// Launch Services does not know about).
    NotFound,
    /// Not running from a bundled `.app`, so there is no login item to
    /// manage.
    Unavailable,
}

impl LoginItemStatus {
    /// Whether launching at login is on (enabled, or waiting for approval).
    pub fn is_on(self) -> bool {
        matches!(
            self,
            LoginItemStatus::Enabled | LoginItemStatus::RequiresApproval
        )
    }
}

impl fmt::Display for LoginItemStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LoginItemStatus::Enabled => "enabled",
            LoginItemStatus::RequiresApproval => {
                "requires approval in System Settings \u{203a} Login Items"
            }
            LoginItemStatus::NotRegistered => "not registered",
            LoginItemStatus::NotFound => "not found",
            LoginItemStatus::Unavailable => "unavailable (not running from a .app bundle)",
        })
    }
}

/// Whether the executable at `exe` sits in a `.app` bundle
/// (`<name>.app/Contents/MacOS/<exe>`).
pub fn is_bundled_executable(exe: &Path) -> bool {
    let Some(macos) = exe.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let app = contents.parent();
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && app
            .and_then(Path::extension)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
}

fn running_bundled() -> bool {
    std::env::current_exe().is_ok_and(|exe| is_bundled_executable(&exe))
}

/// The current state of the app's login item.
pub fn status() -> LoginItemStatus {
    if !running_bundled() {
        return LoginItemStatus::Unavailable;
    }
    // SAFETY: plain class method returning the app's service object.
    let service = unsafe { SMAppService::mainAppService() };
    // SAFETY: plain property getter.
    let raw = unsafe { service.status() };
    match raw {
        SMAppServiceStatus::Enabled => LoginItemStatus::Enabled,
        SMAppServiceStatus::RequiresApproval => LoginItemStatus::RequiresApproval,
        SMAppServiceStatus::NotFound => LoginItemStatus::NotFound,
        _ => LoginItemStatus::NotRegistered,
    }
}

/// Registers (`on`) or unregisters the app's login item and returns the
/// new status.
///
/// # Errors
///
/// A readable message when not running bundled or when ServiceManagement
/// refuses.
pub fn set_enabled(on: bool) -> Result<LoginItemStatus, String> {
    if !running_bundled() {
        return Err(LoginItemStatus::Unavailable.to_string());
    }
    // SAFETY: plain class method returning the app's service object.
    let service = unsafe { SMAppService::mainAppService() };
    // SAFETY: both take no arguments and report failure as an NSError.
    let result = unsafe {
        if on {
            service.registerAndReturnError()
        } else {
            service.unregisterAndReturnError()
        }
    };
    match result {
        Ok(()) => Ok(status()),
        Err(e) => Err(e.localizedDescription().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_executables_are_recognized() {
        assert!(is_bundled_executable(Path::new(
            "/Applications/Fx.app/Contents/MacOS/fx-ui-macos"
        )));
        assert!(!is_bundled_executable(Path::new(
            "/repo/target/debug/fx-ui-macos"
        )));
        assert!(!is_bundled_executable(Path::new("/Contents/MacOS/x")));
        assert!(!is_bundled_executable(Path::new("x")));
    }

    #[test]
    fn on_states() {
        assert!(LoginItemStatus::Enabled.is_on());
        assert!(LoginItemStatus::RequiresApproval.is_on());
        assert!(!LoginItemStatus::NotRegistered.is_on());
        assert!(!LoginItemStatus::NotFound.is_on());
        assert!(!LoginItemStatus::Unavailable.is_on());
    }
}
