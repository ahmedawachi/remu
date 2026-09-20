//! What the operating system will let this process do to the desktop.
//!
//! Only macOS gates remote control behind a persisted, user-granted
//! permission, so only macOS has anything to check here. The other platforms
//! report [`PermissionState::NotRequired`] rather than an invented answer: a
//! Windows UIPI refusal or a missing Wayland portal is a per-session failure
//! that surfaces from [`crate::InputInjector::new`], not a setting a user can
//! grant in advance.

use std::fmt;

/// Whether one OS permission is available to this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionState {
    Granted,
    Denied,
    /// This platform has no such permission; the capability is simply available.
    NotRequired,
    /// The platform has the permission but refuses to answer before it is used.
    Unknown,
}

impl PermissionState {
    /// Whether the capability can be used right now.
    ///
    /// [`PermissionState::Unknown`] counts as usable: the caller should attempt
    /// the operation and report the real failure rather than pre-emptively
    /// disabling a feature that may well work.
    pub fn is_usable(self) -> bool {
        matches!(
            self,
            PermissionState::Granted | PermissionState::NotRequired | PermissionState::Unknown
        )
    }
}

impl fmt::Display for PermissionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            PermissionState::Granted => "granted",
            PermissionState::Denied => "denied",
            PermissionState::NotRequired => "not required",
            PermissionState::Unknown => "unknown",
        };
        f.write_str(text)
    }
}

/// A pane of the macOS Privacy & Security settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrivacyPane {
    Accessibility,
    ScreenRecording,
}

/// Can this process synthesize keyboard and mouse input?
///
/// On macOS this is the Accessibility permission, probed through enigo, which
/// calls `AXIsProcessTrustedWithOptions` with the prompt suppressed. Elsewhere
/// there is no such permission and the answer is always
/// [`PermissionState::NotRequired`].
pub fn accessibility() -> PermissionState {
    #[cfg(target_os = "macos")]
    {
        macos::probe_accessibility(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermissionState::NotRequired
    }
}

/// Can this process read the contents of the screen?
///
/// macOS answers from `CGPreflightScreenCaptureAccess`, which never prompts.
/// Windows has no such permission. On Linux the answer depends on the session:
/// X11 imposes nothing, while Wayland asks the user through a portal at capture
/// time and cannot be queried beforehand, so that case reports
/// [`PermissionState::Unknown`].
pub fn screen_recording() -> PermissionState {
    #[cfg(target_os = "macos")]
    {
        if core_graphics::access::ScreenCaptureAccess.preflight() {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    }
    #[cfg(target_os = "windows")]
    {
        PermissionState::NotRequired
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            PermissionState::Unknown
        } else {
            PermissionState::NotRequired
        }
    }
}

/// Asks macOS to show the Accessibility permission prompt.
///
/// Call this from a user action, never on startup: macOS shows the dialog at
/// most once per process and ignoring it teaches the user to dismiss it. On
/// every other platform this does nothing, because there is nothing to grant.
pub fn request_accessibility() {
    #[cfg(target_os = "macos")]
    {
        let state = macos::probe_accessibility(true);
        tracing::debug!(%state, "requested macOS Accessibility permission");
    }
    #[cfg(not(target_os = "macos"))]
    {
        tracing::debug!("input injection needs no granted permission on this platform");
    }
}

/// Opens the system settings pane where the user can grant `pane`.
///
/// macOS only: no Windows or Linux settings page governs input injection or
/// screen capture, so this logs and returns there. Failure to launch the
/// settings app is logged rather than returned — the caller's next move is to
/// show the same manual instructions either way.
pub fn open_privacy_settings(pane: PrivacyPane) {
    #[cfg(target_os = "macos")]
    {
        let anchor = match pane {
            PrivacyPane::Accessibility => "Privacy_Accessibility",
            PrivacyPane::ScreenRecording => "Privacy_ScreenCapture",
        };
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        // `open` returns as soon as the settings app is told to come forward,
        // so waiting for it does not block on the user reading the pane.
        match std::process::Command::new("open").arg(&url).status() {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!(%status, %url, "settings app refused to open the pane"),
            Err(error) => tracing::warn!(%error, %url, "could not launch the settings app"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        tracing::debug!(
            ?pane,
            "this platform has no privacy pane for remote control"
        );
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::PermissionState;

    /// Probes Accessibility by asking enigo for a connection it can only make
    /// with that permission.
    ///
    /// Enigo's own `has_permission` is not re-exported, and this crate forbids
    /// unsafe code, so constructing the connection *is* the check. It is cheap:
    /// when the permission is missing enigo returns before touching CoreGraphics.
    pub fn probe_accessibility(prompt: bool) -> PermissionState {
        let settings = enigo::Settings {
            open_prompt_to_get_permissions: prompt,
            // A probe must never touch the keyboard state of whoever is sitting
            // at this machine, not even on drop.
            release_keys_when_dropped: false,
            ..enigo::Settings::default()
        };
        match enigo::Enigo::new(&settings) {
            Ok(_) => PermissionState::Granted,
            Err(enigo::NewConError::NoPermission) => PermissionState::Denied,
            Err(error) => {
                tracing::warn!(%error, "could not determine Accessibility permission");
                PermissionState::Unknown
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_accessibility_as_required_on_macos_and_not_elsewhere() {
        let state = accessibility();
        if cfg!(target_os = "macos") {
            assert_ne!(
                state,
                PermissionState::NotRequired,
                "macOS always gates input injection behind Accessibility"
            );
        } else {
            assert_eq!(state, PermissionState::NotRequired);
        }
    }

    #[test]
    fn answers_screen_recording_without_prompting() {
        // The macOS preflight is documented as non-prompting, so this test is
        // safe to run unattended; it would hang a CI machine if it were not.
        let state = screen_recording();
        if cfg!(target_os = "macos") {
            assert!(matches!(
                state,
                PermissionState::Granted | PermissionState::Denied
            ));
        } else {
            assert!(state.is_usable());
        }
    }

    #[test]
    fn only_a_denied_permission_blocks_a_caller() {
        assert!(!PermissionState::Denied.is_usable());
        assert!(PermissionState::Granted.is_usable());
        assert!(PermissionState::NotRequired.is_usable());
        // Unknown must not disable a feature: a Wayland host can still capture.
        assert!(PermissionState::Unknown.is_usable());
    }

    #[test]
    fn describes_each_state_in_words_for_the_status_bar() {
        assert_eq!(PermissionState::Denied.to_string(), "denied");
        assert_eq!(PermissionState::NotRequired.to_string(), "not required");
    }
}
