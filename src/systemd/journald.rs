use std::{
    os::{fd::AsFd, unix::net::UnixDatagram},
    sync::OnceLock,
};

use rustix::{
    fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create},
    io::{self, Errno},
};

use crate::{error::*, utils::send_fds::SendFds};

static JOURNAL: OnceLock<JournalWriter> = OnceLock::new();

const JOURNALD_PATH: &str = "/run/systemd/journal/socket";

pub fn init_journald() -> Result<()> {
    let writer = JournalWriter::new()?;
    JOURNAL.set(writer).ok().ctx("Already initialized")
}

pub fn log(level: LogLevel, message: &str) -> Result<()> {
    JOURNAL
        .get()
        .ctx("Journald not initialized")?
        .log(level, message)
}

#[repr(u8)]
#[allow(dead_code)]
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
        let socket = UnixDatagram::unbound().ctx("Cannot open a datagram socket")?;
        socket
            .connect(JOURNALD_PATH)
            .ctx("Cannot connect to notifier socket")?;

        Ok(Self { socket })
    }

    fn send(&self, payload: &[u8]) -> Result<()> {
        match self.socket.send(payload) {
            Ok(_) => Ok(()),

            Err(e) if Errno::from_io_error(&e) == Some(Errno::MSGSIZE) => self
                .send_large(payload)
                .ctx("Failed to transmit large payload"),

            Err(other) => whatever!("socket error: {}", other),
        }
    }

    fn send_large(&self, payload: &[u8]) -> Result<()> {
        let fd = memfd_create(
            "journald-large-payload-carrier",
            MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC,
        )
        .ctx("Failed to create memfd")?;

        io::write(&fd, payload).ctx("Failed to write payload to memfd")?;

        fcntl_add_seals(&fd, SealFlags::all()).ctx("Failed to seal memfd")?;

        self.socket
            .send_fds(&[fd.as_fd()])
            .ctx("Failed to send fd on socket")?;

        Ok(())
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
