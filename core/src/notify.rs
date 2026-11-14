//! Desktop notifications over org.freedesktop.Notifications.

use crate::paths::APP_ID;
use std::collections::HashMap;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::Value;

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// Shows "N songs copied" with an Eject action. `on_eject` runs when the action is chosen.
pub fn songs_copied(count: usize, on_eject: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        let _ = send(count, on_eject);
    });
}

fn send(count: usize, on_eject: impl FnOnce()) -> zbus::Result<()> {
    let conn = Connection::session()?;
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(DEST)?
        .path(PATH)?
        .build();
    let signals = MessageIterator::for_match_rule(rule, &conn, Some(16))?;

    let summary = match count {
        1 => "1 song copied to the Sansa".to_string(),
        n => format!("{n} songs copied to the Sansa"),
    };
    let hints = HashMap::from([("desktop-entry", Value::from(APP_ID))]);
    let proxy = Proxy::new(&conn, DEST, PATH, DEST)?;
    let id: u32 = proxy.call(
        "Notify",
        &(
            "ClipPlus Manager",
            0u32,
            APP_ID,
            summary,
            "",
            vec!["eject", "Eject"],
            hints,
            10_000i32,
        ),
    )?;

    for message in signals.flatten() {
        let header = message.header();
        let member = header.member().map(|m| m.as_str());
        let body = message.body();
        if member == Some("ActionInvoked")
            && let Ok((got, action)) = body.deserialize::<(u32, String)>()
            && got == id
            && action == "eject"
        {
            on_eject();
            break;
        }
        if member == Some("NotificationClosed")
            && let Ok((got, _)) = body.deserialize::<(u32, u32)>()
            && got == id
        {
            break;
        }
    }
    Ok(())
}
