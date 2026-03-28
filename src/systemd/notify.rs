use std::{os::unix::net::UnixDatagram, path::PathBuf};

use eyre::{Context, Result};

pub struct Notifier {
    socket: UnixDatagram,
}

envy::define_env!(NotifySocket(PathBuf) = #raw "NOTIFY_SOCKET");

impl Notifier {
    pub fn from_env(env: &impl envy::Get) -> Result<Self> {
        let path = env
            .get::<NotifySocket>()
            .context("Cannot find a notify target in environment")?;

        let socket = UnixDatagram::unbound().context("Cannot open a datagram socket")?;
        socket
            .connect(&*path)
            .context("Cannot connect to notifier socket")?;

        Ok(Self { socket })
    }

    fn notify(&mut self, payload: &str) -> Result<()> {
        self.socket
            .send(payload.as_bytes())
            .context("Sending notification on socket failed")?;

        Ok(())
    }

    pub fn notify_ready(&mut self) -> Result<()> {
        self.notify("READY=1")
    }

    pub fn notify_stopping(&mut self) -> Result<()> {
        self.notify("STOPPING=1")
    }
}
