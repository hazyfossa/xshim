mod env_definitions;
mod frame;
mod runtime_dir;
mod systemd;
mod utils;
mod xauthority;

use std::{
    io::{BufRead, BufReader, PipeReader, pipe},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use argh::FromArgs;

use crate::{
    env_definitions::*,
    frame::environment::{Env, EnvDiff, EnvOs},
    runtime_dir::RuntimeDirManager,
    systemd::{journald, notify::Notifier},
    utils::{
        fd::{CommandFdExt, FdContext, SimpleFdContext},
        subprocess::{ChildWithCleanup, spawn_with_cleanup},
        warn::WarnExt,
    },
    xauthority::XAuthorityManager,
};

// You may want to change this appropriately if you're making a package
const DEFAULT_XORG_PATH: &str = "/usr/lib/Xorg";

struct DisplayReceiver(PipeReader);

impl DisplayReceiver {
    fn setup<'a>(fd_ctx: &mut SimpleFdContext, command: &'a mut Command) -> Result<Self> {
        let (display_rx, display_tx) = pipe().context("Failed to open pipe for display fd")?;

        let display_tx_passed = fd_ctx.pass(display_tx.into())?;

        command.args(["-displayfd", &display_tx_passed.as_raw_fd().to_string()]);

        Ok(Self(display_rx))
    }

    fn blocking_wait(self) -> Result<Display> {
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

fn spawn_server(
    path: &Path,
    authority: PathBuf,
    vt: VtNumber,
    seat: Option<Seat>,
) -> Result<(DisplayReceiver, ChildWithCleanup)> {
    let mut fd_ctx = FdContext::new(1);

    let mut command = Command::new(path);

    if let Some(seat) = seat {
        command.args(["-seat", &seat]);
    }

    // TODO: is it still good practice to add -novtswitch
    // even though xshim does not perform work on tty/vt
    // and neither does it call logind

    command
        .arg(format!("vt{}", vt.to_string()))
        .args(["-auth".into(), authority])
        .args(["-nolisten", "tcp"])
        .args(["-background", "none", "-noreset", "-keeptty", "-novtswitch"])
        .args(["-verbose", "3", "-logfile", "/dev/null"])
        .envs([("XORG_RUN_AS_USER_OK", "1")]); // TODO: relevant?

    let display_rx = DisplayReceiver::setup(&mut fd_ctx, &mut command)?;
    command.with_fd_context(fd_ctx);

    // TODO: proxy logs
    let child = spawn_with_cleanup(&mut command).context("Failed to spawn Xorg")?;

    Ok((display_rx, child))
}

#[derive(FromArgs)]
/// Run Xorg like a wayland session
struct Args {
    #[argh(positional)]
    /// client executable
    client: PathBuf,
    #[argh(option, default = "DEFAULT_XORG_PATH.into()")]
    /// override the path used to exec Xorg
    xorg_path: PathBuf,
    #[argh(switch)]
    /// omit XAuthority locking (use at your own risk!)
    skip_locks: bool,
    #[argh(switch)]
    /// use systemd notifications
    notify: bool,
}

// TODO: display this somewhere
fn _help_skip_locks() {
    println!(
        "Using this switch will omit standard XAuthority locking.

    This marginally increases performance, but could lead to conflicts
    if something else tries to interact with XAuthority alongside xshim.

    Use at your own risk!"
    )
}

// TODO: xinit compat mode

fn main() -> Result<()> {
    let env = EnvOs::new_view();
    let args: Args = argh::from_env();

    // TODO: make this non-fatal, fallback to stderr
    journald::init_journald().context("Failed to initialize journald client")?;

    // TODO: add an unsafe option to try and determine one anyway
    let vt = env.get::<VtNumber>().context("Cannot find a VT number allocated for current session. Are you running this from a correct place?")?;

    let seat = env
        .get::<Seat>()
        .context("Cannot find a seat for the current session")
        .warn();

    // TODO: is this even relevant?
    let window_path = WindowPath::previous_plus_vt(&env, &vt);

    let mut notifier = match args.notify {
        true => Some(Notifier::from_env(&env).context("Failed to setup systemd notifications")?),
        false => None,
    };

    let rt_dir_manager =
        RuntimeDirManager::from_env(&env).context("Cannot setup runtime dir manager")?;

    let runtime_dir = rt_dir_manager
        .create(&format!("xshim-{}", *vt))
        .context("Failed to create runtime directory")?;

    let authority_manager = XAuthorityManager::new(runtime_dir, args.skip_locks)
        .context("Cannot setup XAuthority manager")?;

    let server_authority = authority_manager
        .setup_server()
        .context("Failed to define server authority")?;

    let (future_display, _xorg_child) = spawn_server(&args.xorg_path, server_authority, vt, seat)
        .context("Failed to spawn Xorg")?;

    let display = future_display.blocking_wait()?;

    let client_authority = authority_manager
        .setup_client(&display)
        .context("failed to define client authority")?;

    // NOTE: RuntimeDir persists until main is closed
    authority_manager.finish();

    let mut client_child = spawn_with_cleanup(
        Command::new(args.client).envs((display, client_authority, window_path).to_env_diff()),
    )
    .context("Failed to spawn client")?;

    if let Some(ref mut notifier) = notifier {
        notifier
            .notify_ready()
            .context("Failed to signal readiness")?;
    }

    // TODO: is there a point in waiting on Xorg? Client should always close if XServer drops, right?
    // ...will systemd reap the zombie as part of session logout?

    client_child
        .wait()
        .context("Error while waiting on client")?;

    if let Some(ref mut notifier) = notifier {
        let _best_effort = notifier.notify_stopping();
    }

    Ok(())
}
