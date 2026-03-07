use std::os::unix::net::UnixDatagram;

use anyhow::{Context, Result, bail};
use rustix::{
    fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create},
    io::Errno,
};

const JOURNALD_PATH: &str = "/run/systemd/journal/socket";

#[repr(u8)]
pub enum LogLevel {
    Debug = 0,
    Informational,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
}

fn write_length_encoded(buffer: &mut Vec<u8>, value: &[u8]) {
    buffer.push(b'\n');
    // Reserve the length tag
    buffer.extend_from_slice(&[0; 8]);
    let value_start = buffer.len();
    buffer.extend_from_slice(value);
    let value_end = buffer.len();
    // Fill the length tag
    let length_bytes = ((value_end - value_start) as u64).to_le_bytes();
    buffer[value_start - 8..value_start].copy_from_slice(&length_bytes);
    buffer.push(b'\n');
}

fn write_journal_value(buffer: &mut Vec<u8>, value: &[u8]) {
    if value.contains(&b'\n') {
        write_length_encoded(buffer, value);
    } else {
        buffer.push(b'=');
        buffer.extend_from_slice(value);
        buffer.push(b'\n');
    }
}

pub struct JournalWriter {
    socket: UnixDatagram,
}

impl JournalWriter {
    pub fn new() -> Result<Self> {
        let socket = UnixDatagram::unbound().context("Cannot open a datagram socket")?;
        socket
            .connect(JOURNALD_PATH)
            .context("Cannot connect to notifier socket")?;

        Ok(Self { socket })
    }

    fn send(&self, payload: &[u8]) -> Result<()> {
        match self.socket.send(payload) {
            Ok(_) => Ok(()),

            Err(e) if Errno::from_io_error(&e) == Some(Errno::MSGSIZE) => self
                .send_large(payload)
                .context("Failed to transmit large payload"),

            Err(other) => bail!(other),
        }
    }

    fn send_large(&self, payload: &[u8]) -> Result<()> {
        let fd = memfd_create(
            "journald-large-payload-carrier",
            MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC,
        )
        .context("Failed to create memfd")?;

        fcntl_add_seals(fd, SealFlags::all()).context("Failed to seal memfd")?;

        // TODO: send fd
        todo!()
    }

    pub fn log(&self, level: LogLevel, message: &str) -> Result<()> {
        let mut payload = Vec::new();

        payload.extend_from_slice(b"MESSAGE");
        write_journal_value(&mut payload, message.as_bytes());

        payload.extend_from_slice(b"PRIORITY=");
        payload.extend_from_slice((level as u8).to_string().as_bytes());

        self.send(&payload)
    }
}
