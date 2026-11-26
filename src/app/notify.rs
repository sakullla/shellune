//! Desktop notifications for terminals in the background (R2).
//!
//! GPUI has no notification API, so the window hands a finished [`SystemNotice`] to a
//! [`Notifier`]. The real one goes through `notify-rust` (the freedesktop bus on
//! Linux and the BSDs, the notification centre on macOS, toasts on Windows); tests
//! use [`RecordingNotifier`], which keeps what it was asked to show and can be told
//! to fail, so the fallback to a tab mark is exercised without a desktop.
//!
//! Showing is blocking on some platforms, so the window calls [`Notifier::show`] on
//! a background thread and only reacts to its result.

#[cfg(test)]
use std::sync::{Arc, Mutex};

/// One notification, already decided on and worded by the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemNotice {
    /// The first line: the program's title, or what happened.
    pub title: String,
    /// The text under it.
    pub body: String,
}

/// Something that can put a notification on the desktop.
pub trait Notifier: Send + Sync {
    /// Shows one notification.
    ///
    /// # Errors
    ///
    /// Returns why the platform did not show it: no notification service, a
    /// permission the user withheld, a bus that is not running.
    fn show(&self, notice: &SystemNotice) -> Result<(), String>;
}

/// The platform's own notifications, through `notify-rust`.
#[derive(Debug, Default)]
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn show(&self, notice: &SystemNotice) -> Result<(), String> {
        notify_rust::Notification::new()
            .appname(crate::app::product::NAME)
            .summary(&notice.title)
            .body(&notice.body)
            .show()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// A notifier that records instead of showing, for tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    shown: Mutex<Vec<SystemNotice>>,
    failing: Mutex<bool>,
}

#[cfg(test)]
impl RecordingNotifier {
    /// A recording notifier behind the shared handle the window keeps.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// What it was asked to show, oldest first.
    #[must_use]
    pub fn shown(&self) -> Vec<SystemNotice> {
        self.shown
            .lock()
            .map(|shown| shown.clone())
            .unwrap_or_default()
    }

    /// Makes every later [`Notifier::show`] fail, as a desktop without a
    /// notification service would.
    pub fn set_failing(&self, failing: bool) {
        if let Ok(mut slot) = self.failing.lock() {
            *slot = failing;
        }
    }
}

#[cfg(test)]
impl Notifier for RecordingNotifier {
    fn show(&self, notice: &SystemNotice) -> Result<(), String> {
        if self.failing.lock().map(|failing| *failing).unwrap_or(false) {
            return Err("no notification service".to_owned());
        }
        if let Ok(mut shown) = self.shown.lock() {
            shown.push(notice.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_notifier_keeps_what_it_showed_and_can_fail() {
        let notifier = RecordingNotifier::default();
        let notice = SystemNotice {
            title: "t".into(),
            body: "b".into(),
        };
        assert_eq!(notifier.show(&notice), Ok(()));
        notifier.set_failing(true);
        assert!(notifier.show(&notice).is_err());
        assert_eq!(notifier.shown(), vec![notice]);
    }
}
