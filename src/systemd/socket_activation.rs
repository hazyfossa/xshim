use std::{
    num::NonZeroI32,
    os::fd::{FromRawFd, RawFd},
};

use envy::{Get, container::OsEnv, define_env};
use eyre::{Result, ensure};
use rustix::process;

const SD_LISTEN_FDS_START: i32 = 3;

define_env!(ListenPID(NonZeroI32) = "LISTEN_PID");
define_env!(ListenFDs(i32) = "LISTEN_FDS");

// TODO: unset from passed env view?
fn listen_fds() -> Result<Vec<RawFd>> {
    let env = OsEnv::new_view();

    ensure!(
        env.get::<ListenPID>()?.0 == process::getpid().as_raw_nonzero(),
        "LISTEN_PID does not match the current PID"
    );

    let listen_fds_len = env.get::<ListenFDs>()?.0;

    #[allow(clippy::useless_conversion)]
    Ok((SD_LISTEN_FDS_START..=listen_fds_len)
        .map(RawFd::from)
        .collect())
}

/// Safety: this function will cast an FD to a rust socket type
/// The caller must ensure systemd provides a socket of a correct type
// TODO: check socket type at runtime?
pub unsafe fn listen_fd_simple<T: FromRawFd>() -> Result<T> {
    let fds = listen_fds()?;

    ensure!(fds.len() == 1, "Expected 1 FD, got {}", fds.len());

    let fd = fds[0];
    let cast = unsafe { T::from_raw_fd(fd) };
    Ok(cast)
}
