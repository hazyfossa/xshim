use std::{
    num::NonZeroI32,
    os::fd::{FromRawFd, RawFd},
};

use envy::{Env, container::OsEnv, define_env};
use rustix::process;
use snafu::{Snafu, ensure};

use crate::error::*;

const SD_LISTEN_FDS_START: i32 = 3;

define_env!(ListenPID(NonZeroI32) = "LISTEN_PID");
define_env!(ListenFDs(i32) = "LISTEN_FDS");

#[derive(Debug, Snafu)]
pub enum SocketActivationError {
    #[snafu(display("LISTEN_PID does not match the current PID"))]
    ListenPidMismatch,
    #[snafu(display("Expected {expected} FDs, got {got}"))]
    FdLenMismatch { expected: i32, got: usize },
    #[snafu(transparent)]
    EnvError { source: envy::Error },
}

// TODO: unset from passed env view?
fn listen_fds() -> Result<Vec<RawFd>, SocketActivationError> {
    let env = OsEnv::new_view();

    ensure!(
        env.get::<ListenPID>()?.0 == process::getpid().as_raw_nonzero(),
        ListenPidMismatchSnafu
    );

    let listen_fds_len = env.get::<ListenFDs>()?.0;

    Ok((SD_LISTEN_FDS_START..=listen_fds_len)
        .map(RawFd::from)
        .collect())
}

/// Safety: this function will cast an FD to a rust socket type
/// The caller must ensure systemd provides a socket of a correct type
// TODO: check socket type at runtime?
pub unsafe fn listen_fd_simple<T: FromRawFd>() -> Result<T, SocketActivationError> {
    let fds = listen_fds()?;

    ensure!(
        fds.len() == 1,
        FdLenMismatchSnafu {
            expected: 1,
            got: fds.len()
        }
    );

    let fd = fds[0];
    let cast = unsafe { T::from_raw_fd(fd) };
    Ok(cast)
}
