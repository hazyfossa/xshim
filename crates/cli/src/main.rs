mod context;
mod systemd;
mod utils;

use std::{
    env::home_dir,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{ChildStderr, Command},
};

use argh::FromArgs;
use envy::{Set, define_env, diff};
use eyre::{Context as ErrorContext, ContextCompat as ErrorContextCompat, Result};
use freedesktop_session_parser::{SessionKind, get_session_entry};
use tokio::io::AsyncBufReadExt;

use crate::{
    context::ContextMode,
    systemd::{
        journald::{self, LogLevel},
        notify::Notifier,
    },
    utils::{path::EnsureExistsExt, warn::WarnExt},
};

use lib::{Seat, VtNumber, subprocess::CleanupExt};
use libxshim as lib;

#[derive(FromArgs)]
#[argh(subcommand, name = "run")]
/// Run a client executable.
struct DirectMode {
    /// client executable
    #[argh(positional)]
    executable: PathBuf,
}

impl DirectMode {
    fn run(self) -> Result<Command> {
        Ok(Command::new(self.executable))
    }
}

define_env!(pub WindowPath(String) = "WINDOWPATH");

impl WindowPath {
    fn previous_plus_vt(env: &impl envy::Get, vt: &VtNumber) -> Self {
        let previous = env.get::<Self>();
        Self(match previous {
            Ok(path) => format!("{}:{}", *path, **vt),
            Err(_) => vt.to_string(),
        })
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "xinit")]
/// Xinit compatibility mode.
struct XinitCompatMode {}

// TODO: support XSERVERRC? Requires changes to mode trait
define_env!(pub XinitRC(PathBuf) = #raw "XINITRC");

impl XinitCompatMode {
    fn run_ext(self, vt: Option<&VtNumber>, env: &impl envy::Get) -> Result<Command> {
        let rc_env = env.get::<XinitRC>().map(|var| var.0);

        let rc_user = || {
            home_dir()
                .context("cannot find the user home directory")?
                .join(".xinitrc")
                .ensure_exists()
        };

        let rc_system = || PathBuf::from("/etc/X11/xinit/xinitrc").ensure_exists();

        let client_path = rc_env.or_else(|_| rc_user()).or_else(|_| rc_system()).ok();

        let mut client_command = match client_path {
            None => {
                warn!("Cannot find xinit RC, using xterm as fallback client");
                let mut xterm = Command::new("xterm");
                xterm.args(["-geometry", "+1+1", "-n", "login"]);
                xterm
            }

            Some(path) => {
                let permissions = fs::metadata(&path)
                    .context("Cannot find client executable")?
                    .permissions();

                let is_executable = permissions.mode() & 0o111 != 0;

                match is_executable {
                    true => Command::new(&path),
                    false => {
                        let mut shell = Command::new("/bin/sh");
                        shell.arg(path);
                        shell
                    }
                }
            }
        };

        if let Some(vt) = vt {
            client_command.set(WindowPath::previous_plus_vt(env, vt));
        }

        Ok(client_command)
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

impl SessionMode {
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
    Ok(OsEnv::new_view().into())
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

fn logger_task(stderr: ChildStderr) -> Result<impl Future<Output = Result<()>>> {
    let stderr = tokio::process::ChildStderr::from_std(stderr)
        .context("Failed to set stderr pipe as async")?;

    let mut reader = tokio::io::BufReader::new(stderr).lines();
    let writer = journald::JournalWriter::new()?;

    Ok(async move {
        while let Some(line) = reader.next_line().await? {
            // TODO: parse log level
            writer.log(LogLevel::Notice, &line)?;
        }
        Ok(())
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    simple_eyre::install()?;

    let args: Args = argh::from_env();
    let env = resolve_env(&args)
        .await
        .context("Failed to resolve environment")?;

    // TODO: make this non-fatal, fallback to stderr
    journald::init().context("Failed to initialize journald client")?;

    let context = context::aqquire(&args)
        .await
        .context("Failed to aqquire session context")?;

    let mut notifier = match args.notify {
        true => Some(Notifier::from_env(&env).context("Failed to setup systemd notifications")?),
        false => None,
    };

    let mut client_command = match args.mode {
        ModeSubcommand::Direct(mode) => mode.run(),
        ModeSubcommand::Session(mode) => mode.run(),
        ModeSubcommand::XinitCompat(mode) => mode.run_ext(context.vt_number.as_ref(), &env),
    }?;

    let xshim = libxshim::setup_xorg_with_settings(
        libxshim::Settings::builder()
            .env(env)
            .maybe_path(args.xorg_path)
            .extra_args(args.xorg_args)
            .maybe_vt(context.vt_number)
            .maybe_seat(context.seat)
            .unsafe_skip_xauth_locks(args.skip_locks)
            .build(),
    )
    .context("Failed to setup Xorg")?;

    client_command.apply((
        xshim.client_env,
        (diff::unset::<VtNumber>(), diff::unset::<Seat>()),
    ));

    if let Some(context_env) = context.env_diff {
        client_command.apply(context_env.into_diff());
    }

    let mut client_child = client_command
        .with_cleanup()
        .spawn()
        .context("Failed to spawn client")?;

    if let Some(ref mut notifier) = notifier {
        notifier
            .notify_ready()
            .context("Failed to signal readiness")
            .warn();
    }

    // TODO: is there a point in waiting on Xorg? Client should always close if XServer drops, right?
    // ...will systemd reap the zombie as part of session logout?
    client_child.wait().unwrap();

    if let Some(ref mut notifier) = notifier {
        let _best_effort = notifier.notify_stopping();
    }

    Ok(())
}
