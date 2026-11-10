//! One running app. A second `sansa-dl <link>` hands its links to the first.

use std::sync::mpsc::{Receiver, Sender, channel};
use zbus::blocking::{Connection, Proxy, connection};
use zbus::fdo::{RequestNameFlags, RequestNameReply};

pub const BUS_NAME: &str = "dev.project516.SansaDl";
pub const OBJECT_PATH: &str = "/dev/project516/SansaDl";

/// What another process asks of the running window.
pub enum Command {
    /// Links to add. An empty list only raises the window.
    Open(Vec<String>),
    Eject,
}

struct Service {
    opened: Sender<Command>,
}

#[zbus::interface(name = "dev.project516.SansaDl")]
impl Service {
    /// Links to add. An empty list only raises the window.
    fn open(&self, urls: Vec<String>) {
        let _ = self.opened.send(Command::Open(urls));
    }

    fn eject(&self) {
        let _ = self.opened.send(Command::Eject);
    }
}

#[derive(Clone)]
pub struct Instance {
    conn: Connection,
}

pub enum Role {
    /// Without a session bus there is no instance to hand links to.
    Primary(Option<(Instance, Receiver<Command>)>),
    Forwarded,
}

/// Takes the bus name, or forwards `urls` to whoever has it.
pub fn claim(urls: &[String], restart: bool) -> Role {
    let (opened_tx, opened) = channel();
    let built = connection::Builder::session()
        .and_then(|b| b.serve_at(OBJECT_PATH, Service { opened: opened_tx }))
        .and_then(|b| b.build());
    let conn = match built {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("sansa-dl: no single-instance service: {e}");
            return Role::Primary(None);
        }
    };
    // Without DoNotQueue a second process could replace the first owner.
    let mut requested = conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into());
    if restart {
        for _ in 0..100 {
            if !matches!(
                requested,
                Ok(RequestNameReply::Exists) | Err(zbus::Error::NameTaken)
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            requested = conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into());
        }
    }
    match requested {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            Role::Primary(Some((Instance { conn }, opened)))
        }
        Ok(_) | Err(zbus::Error::NameTaken) => match forward(urls) {
            Ok(()) => Role::Forwarded,
            Err(e) => {
                eprintln!("sansa-dl: could not reach the running window: {e}");
                Role::Primary(None)
            }
        },
        Err(e) => {
            eprintln!("sansa-dl: no single-instance service: {e}");
            Role::Primary(None)
        }
    }
}

fn forward(urls: &[String]) -> zbus::Result<()> {
    let conn = Connection::session()?;
    let proxy = Proxy::new(&conn, BUS_NAME, OBJECT_PATH, BUS_NAME)?;
    proxy.call::<_, _, ()>("Open", &(urls,))
}

/// Asks the running window to eject the card.
pub fn eject_running() -> zbus::Result<()> {
    let conn = Connection::session()?;
    let proxy = Proxy::new(&conn, BUS_NAME, OBJECT_PATH, BUS_NAME)?;
    proxy.call::<_, _, ()>("Eject", &())
}

impl Instance {
    /// Lets a new process take over, for a restart.
    pub fn release(&self) {
        let _ = self.conn.release_name(BUS_NAME);
    }
}
