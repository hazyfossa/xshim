mod env_definitions;
mod error;
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

use argh::FromArgs;
use enum_dispatch::enum_dispatch;
use envy::{Env, container::EnvOs, diff::Diff};

use crate::{
    env_definitions::*,
    error::*,
    runtime_dir::RuntimeDirManager,
    systemd::{journald, notify::Notifier},
    utils::{
        fd::{CommandFdExt, FdContext, SimpleFdContext},
        subprocess::{ChildWithCleanup, spawn_with_cleanup},
    },
    xauthority::{ClientAuthorityEnv, XAuthorityManager},
};

// You may want to change this appropriately if you're making a package
const DEFAULT_XORG_PATH: &str = "/usr/lib/Xorg";

struct DisplayReceiver(PipeReader);

impl DisplayReceiver {
    fn setup(fd_ctx: &mut SimpleFdContext, command: &mut Command) -> Result<Self> {
        let (display_rx, display_tx) = pipe().ctx("Failed to open pipe for display fd")?;

        let display_tx_passed = fd_ctx.pass(display_tx.into())?;

        command.args(["-displayfd", &display_tx_passed.as_raw_fd().to_string()]);

        Ok(Self(display_rx))
    }

    fn blocking_wait(self) -> Result<Display> {
        let mut reader = BufReader::new(self.0);
        let mut display_buf = String::new();

        reader
            .read_line(&mut display_buf)
            .ctx("Failed to read display number")?;

        if display_buf.is_empty() {
            whatever!("Internal Xorg error. See logs above for details.")
        }

        let display_number = display_buf
            .trim_end()
            .parse()
            .ctx("Xorg provided invalid display number")?;

        Ok(Display::from_number(display_number))
    }
}

fn spawn_server(
    path: &Path,
    extra_args: &Vec<String>,
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
        .arg(format!("vt{}", *vt))
        .args(["-auth".into(), authority])
        .args(["-nolisten", "tcp"])
        .args(["-background", "none", "-noreset", "-keeptty", "-novtswitch"])
        .args(["-verbose", "3", "-logfile", "/dev/null"])
        .args(extra_args)
        .envs([("XORG_RUN_AS_USER_OK", "1")]); // TODO: relevant?

    let display_rx = DisplayReceiver::setup(&mut fd_ctx, &mut command)?;
    command.with_fd_context(fd_ctx);

    // TODO: proxy logs
    let child = spawn_with_cleanup(&mut command).ctx("Failed to spawn Xorg")?;

    Ok((display_rx, child))
}

type ClientEnv = (Display, ClientAuthorityEnv, WindowPath);

#[enum_dispatch]
trait Mode {
    fn run(self, x_env: ClientEnv) -> Result<Option<ChildWithCleanup>>;
}

#[derive(FromArgs)]
#[argh(subcommand, name = "run")]
/// Run a client executable.
struct DirectMode {
    /// client executable
    #[argh(positional)]
    executable: PathBuf,
}

impl Mode for DirectMode {
    fn run(self, x_env: ClientEnv) -> Result<Option<ChildWithCleanup>> {
        let client_child =
            spawn_with_cleanup(Command::new(self.executable).envs(x_env.to_env_diff()))
                .ctx("Failed to spawn client")?;

        Ok(Some(client_child))
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "session")]
/// Run an xdg session. You should also consider running direct mode
/// from a higher-level session manager.
struct SessionMode {
    /// xdg session name
    #[argh(positional)]
    name: String,
}

impl Mode for SessionMode {
    fn run(self, x_env: ClientEnv) -> Result<Option<ChildWithCleanup>> {
        todo!()
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "xinit")]
/// Xinit compatibility mode.
struct XinitCompatMode {}

impl Mode for XinitCompatMode {
    fn run(self, x_env: ClientEnv) -> Result<Option<ChildWithCleanup>> {
        todo!()
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "xorg-delegate")]
/// Delegate client lifecycle. Called by a cooperative session manager.
struct DelegateMode {}

impl Mode for DelegateMode {
    fn run(self, x_env: ClientEnv) -> Result<Option<ChildWithCleanup>> {
        todo!()
    }
}

#[derive(FromArgs)]
#[argh(subcommand)]
#[enum_dispatch(Mode)]
enum ModeSubcommand {
    Direct(DirectMode),
    Session(SessionMode),
    XinitCompat(XinitCompatMode),
    Delegate(DelegateMode),
}

#[derive(FromArgs)]
#[rustfmt::skip]
/// Run Xorg like a wayland session
struct Args {
    /// override the path used to exec Xorg
    #[argh(option, default = "DEFAULT_XORG_PATH.into()")] xorg_path: PathBuf,
    
    /// omit XAuthority locking (use at your own risk!)
    #[argh(switch)] skip_locks: bool,
    
    /// use systemd notifications
    #[argh(switch)] notify: bool,

    #[argh(subcommand)] mode: ModeSubcommand,

    // arguments passed verbatim to Xorg
    #[argh(positional)] xorg_args: Vec<String>,
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
    journald::init_journald().ctx("Failed to initialize journald client")?;

    // TODO: add an unsafe option to try and determine one anyway
    let vt = env.get::<VtNumber>().ctx("Cannot find a VT number allocated for current session. Are you running this from a correct place?")?;

    let seat = env
        .get::<Seat>()
        .ctx("Cannot find a seat for the current session")
        .warn();

    // TODO: is this even relevant?
    let window_path = WindowPath::previous_plus_vt(&env, &vt);

    let mut notifier = match args.notify {
        true => Some(Notifier::from_env(&env).ctx("Failed to setup systemd notifications")?),
        false => None,
    };

    let rt_dir_manager =
        RuntimeDirManager::from_env(&env).ctx("Cannot setup runtime dir manager")?;

    let runtime_dir = rt_dir_manager
        .create(&format!("xshim-{}", *vt))
        .ctx("Failed to create runtime directory")?;

    let authority_manager = XAuthorityManager::new(runtime_dir, args.skip_locks)
        .ctx("Cannot setup XAuthority manager")?;

    let server_authority = authority_manager
        .setup_server()
        .ctx("Failed to define server authority")?;

    let (future_display, _xorg_child) =
        spawn_server(&args.xorg_path, &args.xorg_args, server_authority, vt, seat)
            .ctx("Failed to spawn Xorg")?;

    let display = future_display.blocking_wait()?;

    let client_authority = authority_manager
        .setup_client(&display)
        .ctx("failed to define client authority")?;

    // NOTE: RuntimeDir persists until main is closed
    authority_manager.finish();

    let mut client_child = args.mode.run((display, client_authority, window_path))?;

    if let Some(ref mut notifier) = notifier {
        notifier.notify_ready().ctx("Failed to signal readiness")?;
    }

    // TODO: is there a point in waiting on Xorg? Client should always close if XServer drops, right?
    // ...will systemd reap the zombie as part of session logout?
    if let Some(ref mut client_child) = client_child {
        client_child.wait().ctx("Error while waiting on client")?;
    }

    if let Some(ref mut notifier) = notifier {
        let _best_effort = notifier.notify_stopping();
    }

    Ok(())
}
