//! Desktop notifications for terminals in the background (R2).
//!
//! GPUI has no notification API, so the window hands a finished [`SystemNotice`] to a
//! [`Notifier`]. The real one goes through `notify-rust` (the freedesktop bus on
//! Linux and the BSDs, the notification centre on macOS, toasts on Windows); tests
//! use `RecordingNotifier`, which keeps what it was asked to show, can be told to
//! fail, and can "click" what it showed, so the fallback to a tab mark and the way
//! back to the terminal are exercised without a desktop.
//!
//! Showing is a short blocking call, so the window calls [`Notifier::show`] on a
//! background thread. Waiting for a click is not: where the platform reports one, the
//! notifier waits on a thread of its own and calls the click callback from there.
//!
//! What a click does per platform:
//!
//! * freedesktop (Linux, BSD): the notification carries a `default` action and the
//!   `shellune` desktop entry; a click calls the callback, and the window brings
//!   itself and the terminal forward.
//! * macOS: notifications are sent as Shellune's own bundle, so a click brings
//!   Shellune forward and the window's activation goes to the terminal.
//! * Windows: not in this round (R6). Toasts keep notify-rust's default sender, and
//!   coming back to the window soon after is what goes to the terminal.

#[cfg(test)]
use std::sync::{Arc, Mutex};

/// One notification, already decided on and worded by the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemNotice {
    /// The first line: the program's title, or what happened.
    pub title: String,
    /// The text under it, as plain text.
    pub body: String,
}

/// What the window wants done when a notification is clicked. Called at most once,
/// from whichever thread the platform reports the click on.
pub type OnClick = Box<dyn FnOnce() + Send>;

/// Something that can put a notification on the desktop.
pub trait Notifier: Send + Sync {
    /// Shows one notification, and calls `on_click` if the platform reports that it
    /// was clicked. A notifier that cannot tell drops `on_click` without calling it.
    ///
    /// # Errors
    ///
    /// Returns why the platform did not show it: no notification service, a
    /// permission the user withheld, a bus that is not running.
    fn show(&self, notice: &SystemNotice, on_click: OnClick) -> Result<(), String>;
}

/// The platform's own notifications, through `notify-rust`.
#[derive(Debug, Default)]
pub struct DesktopNotifier;

/// The desktop entry the freedesktop server matches a notification to
/// (`packaging/linux/shellune.desktop`).
#[cfg(all(unix, not(target_os = "macos")))]
const DESKTOP_ENTRY: &str = crate::app::product::APP_ID;

/// The bundle identifier notifications are sent as on macOS
/// (`packaging/macos/make-app-bundle.py`).
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "com.shellune.Shellune";

impl Notifier for DesktopNotifier {
    #[cfg(all(unix, not(target_os = "macos")))]
    fn show(&self, notice: &SystemNotice, on_click: OnClick) -> Result<(), String> {
        let handle = notify_rust::Notification::new()
            .appname(crate::app::product::NAME)
            .summary(&notice.title)
            // The body is markup on most servers; the text is the program's, so it
            // is escaped and shows exactly as written (R2).
            .body(&escape_markup(&notice.body))
            .hint(notify_rust::Hint::DesktopEntry(DESKTOP_ENTRY.to_owned()))
            .action("default", crate::app::product::NAME)
            .show()
            .map_err(|error| error.to_string())?;
        // The wait lasts until the notification is clicked or closed; it holds a
        // thread of its own, never one the window's work runs on. Without the
        // thread the notice is still shown; a click then only brings nothing forward.
        let _ = std::thread::Builder::new()
            .name("notification-click".to_owned())
            .spawn(move || {
                handle.wait_for_action(|action| {
                    if action == "default" {
                        on_click();
                    }
                });
            });
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn show(&self, notice: &SystemNotice, on_click: OnClick) -> Result<(), String> {
        static SENDER: std::sync::Once = std::sync::Once::new();
        SENDER.call_once(|| {
            // Without an installed bundle (a development build) this fails and the
            // system's default sender is kept; showing still works.
            let _ = notify_rust::set_application(BUNDLE_ID);
        });
        // A click activates the sending application; the window's activation is
        // what goes to the terminal, so the callback is not needed here.
        drop(on_click);
        notify_rust::Notification::new()
            .appname(crate::app::product::NAME)
            .summary(&notice.title)
            .body(&notice.body)
            .show()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    #[cfg(not(unix))]
    fn show(&self, notice: &SystemNotice, on_click: OnClick) -> Result<(), String> {
        // Clicking a toast to reach the terminal is not in this round (R6).
        drop(on_click);
        notify_rust::Notification::new()
            .appname(crate::app::product::NAME)
            .summary(&notice.title)
            .body(&notice.body)
            .show()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// Escapes the three characters freedesktop body markup gives a meaning to, so a
/// body shows as the plain text it is: `a && b < c` stays `a && b < c`, and a
/// program cannot add a link or an image to a notice in Shellune's name.
#[cfg_attr(not(any(test, all(unix, not(target_os = "macos")))), allow(dead_code))]
#[must_use]
pub fn escape_markup(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// A notifier that records instead of showing, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct RecordingNotifier {
    shown: Mutex<Vec<SystemNotice>>,
    clicks: Mutex<Vec<Option<OnClick>>>,
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

    /// Clicks the notification shown `index`-th, as a person on a freedesktop
    /// desktop would; returns whether there was one left to click.
    pub fn click(&self, index: usize) -> bool {
        let callback = self
            .clicks
            .lock()
            .ok()
            .and_then(|mut clicks| clicks.get_mut(index).and_then(Option::take));
        match callback {
            Some(callback) => {
                callback();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
impl Notifier for RecordingNotifier {
    fn show(&self, notice: &SystemNotice, on_click: OnClick) -> Result<(), String> {
        if self.failing.lock().map(|failing| *failing).unwrap_or(false) {
            return Err("no notification service".to_owned());
        }
        if let Ok(mut shown) = self.shown.lock() {
            shown.push(notice.clone());
        }
        if let Ok(mut clicks) = self.clicks.lock() {
            clicks.push(Some(on_click));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_notifier_keeps_what_it_showed_clicks_once_and_can_fail() {
        let notifier = RecordingNotifier::default();
        let notice = SystemNotice {
            title: "t".into(),
            body: "b".into(),
        };
        let clicked = Arc::new(Mutex::new(false));
        let seen = clicked.clone();
        let on_click: OnClick = Box::new(move || *seen.lock().expect("lock") = true);
        assert_eq!(notifier.show(&notice, on_click), Ok(()));
        assert!(notifier.click(0));
        assert!(*clicked.lock().expect("lock"));
        assert!(!notifier.click(0), "a notification is clicked once");
        notifier.set_failing(true);
        assert!(notifier.show(&notice, Box::new(|| {})).is_err());
        assert_eq!(notifier.shown(), vec![notice]);
    }

    #[test]
    fn body_markup_is_escaped_so_the_text_shows_as_written() {
        assert_eq!(
            escape_markup("make && ./run < in > out"),
            "make &amp;&amp; ./run &lt; in &gt; out"
        );
        assert_eq!(
            escape_markup("<a href=\"x\">click</a>"),
            "&lt;a href=\"x\"&gt;click&lt;/a&gt;"
        );
        assert_eq!(escape_markup("plain"), "plain");
    }
}
