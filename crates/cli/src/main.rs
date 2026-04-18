mod context;
mod systemd;
mod utils;

use std::{env::home_dir, fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

use argh::FromArgs;
use enum_dispatch::enum_dispatch;
use envy::{Get, OsEnv, Set, define_env, diff};
use eyre::{Context as ErrorContext, ContextCompat as ErrorContextCompat, Result};
use freedesktop_session_parser::{SessionKind, get_session_entry};

use crate::{
    context::{ContextMode, Seat, VtNumber},
    systemd::{journald, notify::Notifier},
    utils::{path::EnsureExistsExt, runtime_dir::RuntimeDirManager},
};

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
mod env;
#[cfg(feature = "dbus")]
use env::resolve_env;

#[cfg(not(feature = "dbus"))]
pub async fn resolve_env(_: &Args) -> Result<EnvBuf> {
    Ok(EnvBuf::from_diff(OsEnv::new_view()))
}

#[derive(FromArgs)]
/// Run Xorg like a wayland session
struct Args {
    /// override the path used to exec Xorg
    #[argh(option)]
    xorg_path: Option<PathBuf>,

    #[cfg(feature = "dbus")]
    #[argh(option)]
    /// environment resolution strategy
    env: Option<env::Strategy>,

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
    let env = resolve_env(&args)
        .await
        .context("Failed to resolve environment")?;

    // TODO: make this non-fatal, fallback to stderr
    journald::init_journald().context("Failed to initialize journald client")?;
    simple_eyre::install()?;

    let mut context = context::aqquire(&args)
        .await
        .context("Failed to aqquire session context")?;

    let context_env = context.env_diff.take();

    let mut notifier = match args.notify {
        true => Some(Notifier::from_env(&env).context("Failed to setup systemd notifications")?),
        false => None,
    };

    let rt_dir_manager =
        RuntimeDirManager::from_env(&env).context("Cannot setup runtime dir manager")?;

    // NOTE: RuntimeDir persists until main is closed
    let runtime_dir = rt_dir_manager
        .create(&format!("xshim-{}", context.unique_id()?))
        .context("Failed to create runtime directory")?;

    let xshim = xshim::setup_xorg(
        xshim::Settings::builder()
            .env(env)
            .maybe_path(args.xorg_path)
            .authority_dir(runtime_dir.path.clone())
            .extra_args(args.xorg_args)
            .maybe_vt(context.vt_number)
            .maybe_seat(context.seat)
            .unsafe_skip_locks(args.skip_locks)
            .build(),
    )
    .context("Failed to setup Xorg")?;

    let mut client = args.mode.run()?;

    client.apply((
        xshim.client_env,
        (diff::unset::<VtNumber>(), diff::unset::<Seat>()),
    ));

    if let Some(context_env) = context_env {
        client.apply(context_env.into_diff());
    }

    let mut client_child =
        xshim::subprocess::spawn_with_cleanup(client).context("Failed to spawn client")?;

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
