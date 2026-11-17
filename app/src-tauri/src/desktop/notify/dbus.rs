//! Notifications on Linux, over the D-Bus interface
//! `org.freedesktop.Notifications`.

use std::collections::HashMap;
use std::sync::Arc;

use tauri::AppHandle;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

use super::{Event, Gone, Kind, Text};

/// Key of the button that sends the sign of life.
const RESPOND: &str = "respond";

/// Key of the click on the notification itself.
const DEFAULT: &str = "default";

/// Reason in `NotificationClosed`: closed by the user.
const CLOSED_BY_USER: u32 = 2;

/// Urgency "critical": stays until the user acts. KDE Plasma shows it even
/// during Do Not Disturb; that is intended (see `docs/concept.md`).
const CRITICAL: u8 = 2;

/// Urgency "normal", for answers.
const NORMAL: u8 = 1;

/// The notification service of the session.
pub struct Service {
    proxy: Proxy<'static>,
}

impl Service {
    /// The notification service, or why the session has none.
    pub fn connect(_app: &AppHandle) -> Result<Self, String> {
        Connection::session()
            .and_then(|connection| {
                Proxy::new(
                    &connection,
                    "org.freedesktop.Notifications",
                    "/org/freedesktop/Notifications",
                    "org.freedesktop.Notifications",
                )
            })
            .map_err(|error| error.to_string())
            .map(|proxy| Self { proxy })
    }

    /// Follows the signals of the service, each on a thread of its own.
    pub fn listen(&self, on_event: impl Fn(u32, Event) + Send + Sync + 'static) {
        let on_event = Arc::new(on_event);
        listen_for::<(u32, u32)>(
            &self.proxy,
            on_event.clone(),
            "NotificationClosed",
            |(id, reason)| (id, closed(reason)),
        );
        listen_for::<(u32, String)>(&self.proxy, on_event, "ActionInvoked", |(id, key)| {
            (id, action(&key))
        });
    }

    /// Shows a notification; returns the id the service gave it. A request
    /// is critical, silent and has the button; an answer is normal, with
    /// the sound of the system, and expires as the service decides.
    pub fn show(&self, text: &Text) -> Result<u32, String> {
        let (hints, actions): (HashMap<&str, Value<'_>>, Vec<&str>) = match &text.kind {
            Kind::Request { action } => (
                HashMap::from([
                    ("urgency", Value::U8(CRITICAL)),
                    ("suppress-sound", Value::Bool(true)),
                ]),
                vec![DEFAULT, action, RESPOND, action],
            ),
            Kind::Answer { open } => (
                HashMap::from([("urgency", Value::U8(NORMAL))]),
                vec![DEFAULT, open],
            ),
        };
        let timeout = match text.kind {
            Kind::Request { .. } => 0i32,
            Kind::Answer { .. } => -1i32,
        };
        // The summary is plain text, so the name needs no escaping there.
        self.proxy
            .call(
                "Notify",
                &(
                    "Lebenszeichen",
                    0u32,
                    "",
                    text.title.as_str(),
                    text.hint.as_str(),
                    &actions[..],
                    hints,
                    timeout,
                ),
            )
            .map_err(|error| error.to_string())
    }

    /// Withdraws a notification.
    pub fn close(&self, id: u32) {
        let _: zbus::Result<()> = self.proxy.call("CloseNotification", &(id,));
    }
}

/// Follows one signal of the notification service on a thread of its own.
fn listen_for<T>(
    proxy: &Proxy<'static>,
    on_event: Arc<dyn Fn(u32, Event) + Send + Sync>,
    signal: &'static str,
    read: fn(T) -> (u32, Gone),
) where
    T: serde::de::DeserializeOwned + zbus::zvariant::Type + 'static,
{
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let signals = match proxy.receive_signal(signal) {
            Ok(signals) => signals,
            Err(error) => {
                eprintln!("Signal {signal} unavailable: {error}");
                return;
            }
        };
        for message in signals {
            if let Ok(body) = message.body().deserialize::<T>() {
                let (id, how) = read(body);
                on_event(id, Event::Gone(how));
            }
        }
    });
}

/// `NotificationClosed`: only closing by the user counts; expiry (1),
/// withdrawal by the app (3) and anything else do not.
fn closed(reason: u32) -> Gone {
    if reason == CLOSED_BY_USER {
        Gone::Dismissed
    } else {
        Gone::Vanished
    }
}

/// `ActionInvoked`: the click on the notification and the button count.
fn action(key: &str) -> Gone {
    if key == DEFAULT || key == RESPOND {
        Gone::Clicked
    } else {
        Gone::Vanished
    }
}

#[cfg(test)]
mod tests {
    use super::{action, closed};

    #[test]
    fn signals_become_what_the_person_did() {
        assert!(!closed(1).answers()); // expired
        assert!(closed(2).answers()); // closed by the user
        assert!(!closed(3).answers()); // withdrawn by the app
        assert!(!closed(4).answers()); // other
        assert!(action("default").answers());
        assert!(action("respond").answers());
        assert!(!action("other").answers());
    }
}
