mod context;
mod env_definitions;
mod runtime_dir;
mod systemd;
mod utils;
mod xauthority;

use std::{
    env::home_dir,
    fs,
    io::{BufRead, BufReader, PipeReader, pipe},
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use argh::FromArgs;
use enum_dispatch::enum_dispatch;
use envy::{Get, OsEnv, Set, container::EnvBuf, define_env, diff};
use eyre::{Context as ErrorContext, ContextCompat as ErrorContextCompat, Result, bail};
use freedesktop_session_parser::{SessionKind, get_session_entry};

use crate::{
    context::{ContextMode, SessionContext},
    env_definitions::*,
    runtime_dir::RuntimeDirManager,
    systemd::{journald, notify::Notifier},
    utils::{
        fd::{CommandFdExt, FdContext, SimpleFdContext},
        path::EnsureExistsExt,
        subprocess::{ChildWithCleanup, spawn_with_cleanup},
    },
    xauthority::XAuthorityManager,
};

// You may want to change this if you're making a package
const DEFAULT_XORG_PATH: &str = "/usr/lib/Xorg";

struct DisplayReceiver(PipeReader);

impl DisplayReceiver {
    fn setup(fd_context: &mut SimpleFdContext, command: &mut Command) -> Result<Self> {
        let (display_rx, display_tx) = pipe().context("Failed to open pipe for display fd")?;

        let display_tx_passed = fd_context.pass(display_tx.into())?;

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
    extra_args: &Vec<String>,
    authority: PathBuf,
    context: SessionContext,
) -> Result<(DisplayReceiver, ChildWithCleanup)> {
    let mut fd_context = FdContext::new(1);

    let mut command = Command::new(path);

    if let Some(seat) = context.seat {
        command.args(["-seat", &seat]);
    }

    if let Some(vt) = context.vt_number {
        command.arg(format!("vt{}", *vt)).arg("-novtswitch");
    }

    command
        .args(["-auth".into(), authority])
        .args(["-nolisten", "tcp"])
        .args(["-background", "none", "-noreset", "-keeptty"])
        .args(["-verbose", "3", "-logfile", "/dev/null"])
        .args(extra_args)
        .envs([("XORG_RUN_AS_USER_OK", "1")]);

    let display_rx = DisplayReceiver::setup(&mut fd_context, &mut command)?;
    command.with_fd_context(fd_context);

    // TODO: proxy logs
    let child = spawn_with_cleanup(command).context("Failed to spawn Xorg")?;

    Ok((display_rx, child))
}

#[enum_dispatch]
trait Mode {
    fn run(self) -> Result<Command>;
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
    fn run(self) -> Result<Command> {
        Ok(Command::new(self.executable))
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "xinit")]
/// Xinit compatibility mode.
struct XinitCompatMode {}

// TODO: support XSERVERRC? Requires changes to mode trait
define_env!(pub XinitRC(PathBuf) = #raw "XINITRC");

impl Mode for XinitCompatMode {
    fn run(self) -> Result<Command> {
        let rc_env = OsEnv::new_view().get::<XinitRC>().map(|var| var.0);

        let rc_user = || {
            home_dir()
                .context("cannot find the user home directory")?
                .join(".xinitrc")
                .ensure_exists()
        };

        let rc_system = || PathBuf::from("/etc/X11/xinit/xinitrc").ensure_exists();

        let client_path = match rc_env.or_else(|_| rc_user()).or_else(|_| rc_system()).ok() {
            Some(path) => path,
            None => {
                warn!("Cannot find xinit RC, using xterm as fallback client");
                let mut xterm = Command::new("xterm");
                xterm.args(["-geometry", "+1+1", "-n", "login"]);

                return Ok(xterm);
            }
        };

        let permissions = fs::metadata(&client_path)
            .context("Cannot find client executable")?
            .permissions();

        let is_executable = permissions.mode() & 0o111 != 0;

        Ok(match is_executable {
            true => Command::new(client_path),
            false => {
                let mut shell = Command::new("/bin/sh");
                shell.arg(client_path);
                shell
            }
        })
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "session")]
/// Run an xdg session. You should also consider running direct mode
/// from a higher-level session manager.
pub struct SessionMode {
    /// xdg session name
    #[argh(positional)]
    name: String,
}

impl Mode for SessionMode {
    fn run(self) -> Result<Command> {
        let session = get_session_entry(SessionKind::X11, &self.name)
            .context("Error while reading session definition")?;

        let mut command = Command::new(session.executable);
        if let Some(workdir) = session.working_directory {
            command.current_dir(workdir);
        }

        match session.desktop_names {
            Some(xdg_desktop_list) => {
                if let Some(xdg_desktop) = xdg_desktop_list.as_single_desktop() {
                    command.apply(xdg_desktop);
                }

                command.apply(xdg_desktop_list);
            }
            None => {
                warn!("The session's definition does not provide XDG desktop name(s)");
            }
        };

        Ok(command)
    }
}

#[enum_dispatch(Mode)]
#[derive(FromArgs)]
#[argh(subcommand)]
enum ModeSubcommand {
    Direct(DirectMode),
    XinitCompat(XinitCompatMode),
    Session(SessionMode),
}

#[cfg(feature = "dbus")]
mod resolve_env {
    use super::*;

    #[derive(argh::FromArgValue, Clone)]
    pub enum Strategy {
        /// use unix session environment (shell profile)
        Unix,
        /// use systemd environment
        Systemd,
        /// merge systemd and unix environment. unix values take precedence
        Merge,
    }

    impl Default for Strategy {
        fn default() -> Self {
            Self::Systemd
        }
    }

    fn env_path_merge(primary: &impl envy::Get, secondary: &impl envy::Get) -> Option<PathEnv> {
        let a = primary.get::<PathEnv>().ok();
        let b = secondary.get::<PathEnv>().ok();

        match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, None) => a,
            (None, b) => b,
        }
    }

    pub async fn resolve_env(args: &Args) -> Result<EnvBuf> {
        let mode = &args.env.clone().unwrap_or_default();

        let unix_env = OsEnv::new_view();

        if matches!(mode, Strategy::Unix) {
            return Ok(EnvBuf::from_diff(unix_env));
        }

        let session_bus = zbus::Connection::session()
            .await
            .context("Failed to connect to DBus (session bus)")?;

        let systemd_env = systemd::dbus::environment::SystemdEnvironment::open(&session_bus)
            .await
            .context("Failed to query systemd for environment")?;

        let path = env_path_merge(&systemd_env, &unix_env);

        if matches!(mode, Strategy::Systemd) {
            let mut env = EnvBuf::from_diff(systemd_env);
            env.apply(path);
            return Ok(env);
        };

        let mut merged = EnvBuf::new();
        merged.apply(systemd_env);
        merged.apply(unix_env);
        merged.apply(path);
        Ok(merged)
    }
}

#[cfg(not(feature = "dbus"))]
mod resolve_env {
    use super::*;

    pub async fn resolve_env(_: &Args) -> Result<EnvBuf> {
        Ok(EnvBuf::from_diff(OsEnv::new_view()))
    }
}

#[derive(FromArgs)]
/// Run Xorg like a wayland session
struct Args {
    /// override the path used to exec Xorg
    #[argh(option, default = "DEFAULT_XORG_PATH.into()")]
    xorg_path: PathBuf,

    #[cfg(feature = "dbus")]
    #[argh(option)]
    /// environment resolution strategy
    env: Option<resolve_env::Strategy>,

    #[argh(option)]
    /// session context
    context: Option<ContextMode>,

    /// omit XAuthority locking (use at your own risk!)
    #[argh(switch)]
    skip_locks: bool,

    /// use systemd notifications
    #[argh(switch)]
    notify: bool,

    #[argh(subcommand)]
    mode: ModeSubcommand,

    // arguments passed verbatim to Xorg
    #[argh(positional)]
    xorg_args: Vec<String>,
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();
    let env = resolve_env::resolve_env(&args)
        .await
        .context("Failed to resolve environment")?;

    // TODO: make this non-fatal, fallback to stderr
    journald::init_journald().context("Failed to initialize journald client")?;
    simple_eyre::install()?;

    let mut context = context::aqquire(&args)
        .await
        .context("Failed to aqquire session context")?;

    let context_env = context.env_diff.take();

    let window_path = context
        .vt_number
        .as_ref()
        .map(|vt| WindowPath::previous_plus_vt(&env, vt));

    let mut notifier = match args.notify {
        true => Some(Notifier::from_env(&env).context("Failed to setup systemd notifications")?),
        false => None,
    };

    let rt_dir_manager =
        RuntimeDirManager::from_env(&env).context("Cannot setup runtime dir manager")?;

    let runtime_dir = rt_dir_manager
        .create(&format!("xshim-{}", context.unique_id()?))
        .context("Failed to create runtime directory")?;

    let authority_manager = XAuthorityManager::new(runtime_dir, args.skip_locks)
        .context("Cannot setup XAuthority manager")?;

    let server_authority = authority_manager
        .setup_server()
        .context("Failed to define server authority")?;

    let (future_display, _xorg_child) =
        spawn_server(&args.xorg_path, &args.xorg_args, server_authority, context)
            .context("Failed to spawn Xorg")?;

    let display = future_display.blocking_wait()?;

    let client_authority = authority_manager
        .setup_client(&display)
        .context("Failed to define client authority")?;

    // NOTE: RuntimeDir persists until main is closed
    let _persist = authority_manager.finish();

    let mut client = args.mode.run()?;

    client.apply((
        display,
        client_authority,
        window_path,
        diff::unset::<VtNumber>(),
        diff::unset::<Seat>(),
    ));

    if let Some(context_env) = context_env {
        client.apply(context_env);
    }

    let mut client_child = spawn_with_cleanup(client).context("Failed to spawn client")?;

    if let Some(ref mut notifier) = notifier {
        notifier
            .notify_ready()
            .context("Failed to signal readiness")?;
    }

    // TODO: is there a point in waiting on Xorg? Client should always close if XServer drops, right?
    // ...will systemd reap the zombie as part of session logout?
    client_child.wait().unwrap();

    if let Some(ref mut notifier) = notifier {
        let _best_effort = notifier.notify_stopping();
    }

    Ok(())
}
