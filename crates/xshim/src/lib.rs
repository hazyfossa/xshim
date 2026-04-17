use std::{
    io::{BufRead, BufReader, PipeReader, pipe},
    os::fd::AsRawFd,
    path::PathBuf,
    process::Command,
};

use bon::Builder;
use envy::{OsEnv, container::EnvBuf};
use eyre::{Context, Result, bail};

use crate::{
    env::{Display, WindowPath},
    utils::{
        fd::{CommandFdExt, FdContext, SimpleFdContext},
        subprocess::{ChildWithCleanup, spawn_with_cleanup},
    },
    xauthority::XAuthorityManager,
};

mod env;
mod utils;
mod xauthority;

pub use utils::subprocess;

// You may want to change this if you're making a package
const DEFAULT_XORG_PATH: &str = "/usr/lib/Xorg";

pub type Seat = String;
pub type VtNumber = u32;

struct DisplayReceiver(PipeReader);

impl DisplayReceiver {
    fn setup(fd_context: &mut SimpleFdContext, command: &mut Command) -> Result<Self> {
        let (display_rx, display_tx) = pipe().context("Failed to open pipe for display fd")?;

        let display_tx_passed = fd_context.pass(display_tx.into())?;

        command.args(["-displayfd", &display_tx_passed.as_raw_fd().to_string()]);

        Ok(Self(display_rx))
    }

    // TODO: async
    pub fn blocking_wait(self) -> Result<Display> {
        let mut reader = BufReader::new(self.0);
        let mut display_buf = String::new();

        reader
            .read_line(&mut display_buf)
            .context("Failed to read display number")?;

        if display_buf.is_empty() {
            bail!("Internal Xorg error. See logs above for details.")
        }

        let display_number = display_buf
            .trim_end()
            .parse()
            .context("Xorg provided invalid display number")?;

        Ok(Display::from_number(display_number))
    }
}

fn prepare_xorg(
    settings: &Settings,
    server_authority: PathBuf,
) -> Result<(DisplayReceiver, Command)> {
    let mut fd_context = FdContext::new(1);

    let mut command = Command::new(&settings.path);

    if let Some(seat) = &settings.seat {
        command.args(["-seat", &seat]);
    }

    if let Some(vt) = settings.vt {
        command.arg(format!("vt{}", vt)).arg("-novtswitch");
    }

    command
        .args(["-auth".into(), server_authority])
        .args(["-nolisten", "tcp"])
        .args(["-background", "none", "-noreset", "-keeptty"])
        .args(["-verbose", "3", "-logfile", "/dev/null"])
        .args(&settings.extra_args)
        .envs([("XORG_RUN_AS_USER_OK", "1")]);

    let display_rx = DisplayReceiver::setup(&mut fd_context, &mut command)?;
    command.with_fd_context(fd_context);

    Ok((display_rx, command))
}

#[derive(Builder)]
pub struct Settings {
    authority_dir: PathBuf,
    #[builder(default = DEFAULT_XORG_PATH.into())]
    path: PathBuf,
    env: Option<EnvBuf>,

    #[builder(into)]
    vt: Option<VtNumber>,
    #[builder(into)]
    seat: Option<Seat>,

    #[builder(default)]
    extra_args: Vec<String>,
    #[builder(default = false)]
    unsafe_skip_locks: bool,
}

/// Returns (xorg_child, client_env)
/// Will block the current thread until Xorg provides a display
///
/// Should be called from the context of the session user, *not* the root user
/// (Xorg as root is discouraged)
// TODO: optionally switch user on spawn
pub fn setup_xorg(settings: Settings) -> Result<(ChildWithCleanup, impl envy::diff::Diff)> {
    let authority_manager =
        XAuthorityManager::new(settings.authority_dir.clone(), settings.unsafe_skip_locks)
            .context("Cannot setup XAuthority manager")?;

    let server_authority = authority_manager
        .setup_server()
        .context("Failed to define server authority")?;

    let (future_display, xorg_command) = prepare_xorg(&settings, server_authority)?;
    let xorg = spawn_with_cleanup(xorg_command).context("Failed to spawn Xorg")?;

    let display = future_display.blocking_wait()?;

    let client_authority = authority_manager
        .setup_client(&display)
        .context("Failed to define client authority")?;

    // TODO: we only use this for WindowPath. Is it even relevant?
    let env = settings.env.unwrap_or(EnvBuf::from_diff(OsEnv::new_view()));

    let window_path = settings
        .vt
        .as_ref()
        .map(|vt| WindowPath::previous_plus_vt(&env, vt));

    let client_env = (display, client_authority, window_path);

    Ok((xorg, client_env))
}
