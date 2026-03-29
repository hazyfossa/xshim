mod context;
mod env_definitions;
mod runtime_dir;
mod systemd;
mod utils;
mod xauthority;

use std::{
    env::home_dir,
    ffi::OsStr,
    io::{self, BufRead, BufReader, ErrorKind, PipeReader, pipe},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, net::UnixDatagram},
    },
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use argh::FromArgs;
use enum_dispatch::enum_dispatch;
use envy::{
    Get, OsEnv, Set, Unset,
    container::EnvBuf,
    define_env,
    diff::{self, Diff, Entry},
};
use eyre::{Context as ErrorContext, ContextCompat as ErrorContextCompat, Result, bail};

use crate::{
    context::SessionContext,
    env_definitions::*,
    runtime_dir::RuntimeDirManager,
    systemd::{journald, notify::Notifier},
    utils::{
        fd::{CommandFdExt, FdContext, SimpleFdContext},
        path::EnsureExistsExt,
        subprocess::{ChildWithCleanup, spawn_with_cleanup},
    },
    xauthority::{ClientAuthorityEnv, XAuthorityManager},
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
        command.arg(format!("vt{}", *vt));
        // TODO: should keeptty be here or in global?
        // read on tty control
        command.args(["-keeptty", "-novtswitch"]);
    }

    command
        .args(["-auth".into(), authority])
        .args(["-nolisten", "tcp"])
        .args(["-background", "none", "-noreset"])
        .args(["-verbose", "3", "-logfile", "/dev/null"])
        .args(extra_args)
        .envs([("XORG_RUN_AS_USER_OK", "1")]); // TODO: relevant?

    let display_rx = DisplayReceiver::setup(&mut fd_context, &mut command)?;
    command.with_fd_context(fd_context);

    // TODO: proxy logs
    let child = spawn_with_cleanup(command).context("Failed to spawn Xorg")?;

    Ok((display_rx, child))
}

type ClientEnv = (
    Display,
    ClientAuthorityEnv,
    Option<WindowPath>,
    Unset<VtNumber>,
    Unset<Seat>,
);

enum Client {
    Command(Command),
    Process(Result<ChildWithCleanup, io::Error>),
    None,
}

impl Client {
    fn bind(self) -> Result<ExitStatus> {
        match self {
            // if provided a command, spawn a process
            Client::Command(command) => {
                let process = spawn_with_cleanup(command);
                Self::Process(process).bind()
            }
            // if provided a process, wait on in
            Client::Process(process) => Ok(process
                .context("Failed to spawn client subprocess")?
                .wait()
                .unwrap()),
            // if not dealing with processes, return successs
            Client::None => Ok(ExitStatus::default()),
        }
    }
}

#[enum_dispatch]
trait Mode {
    fn run(self, x_env: ClientEnv) -> Result<Client>;
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
    fn run(self, x_env: ClientEnv) -> Result<Client> {
        let mut command = Command::new(self.executable);
        command.apply(x_env);

        Ok(Client::Command(command))
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "xinit")]
/// Xinit compatibility mode.
struct XinitCompatMode {}

// TODO: support XSERVERRC? Requires changes to mode trait
define_env!(pub XinitRC(PathBuf) = #raw "XINITRC");

impl Mode for XinitCompatMode {
    fn run(self, x_env: ClientEnv) -> Result<Client> {
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

                return Ok(Client::Command(xterm));
            }
        };

        let mut command = Command::new(&client_path);
        command.apply(x_env.clone());

        let client_process = match spawn_with_cleanup(command) {
            Ok(process) => Ok(process),
            // retry with shell
            Err(e) if e.kind() != ErrorKind::PermissionDenied => {
                let mut with_shell = Command::new("/bin/sh");
                with_shell.arg(client_path);
                with_shell.apply(x_env);

                spawn_with_cleanup(with_shell)
            }
            Err(other) => Err(other),
        };

        Ok(Client::Process(client_process))
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "session")]
#[cfg(feature = "session")]
/// Run an xdg session. You should also consider running direct mode
/// from a higher-level session manager.
pub struct SessionMode {
    /// xdg session name
    #[argh(positional)]
    name: String,
}

#[cfg(feature = "session")]
impl Mode for SessionMode {
    fn run(self, x_env: ClientEnv) -> Result<Client> {
        use freedesktop_session_parser::{SessionKind, get_session_entry};

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

        command.apply(x_env);

        Ok(Client::Command(command))
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "xorg-delegate")]
/// Delegate client lifecycle. Called by a cooperative session manager.
struct DelegateMode {
    /// use systemd socket activation
    #[argh(switch)]
    systemd: bool,

    /// use socket on path
    #[argh(option)]
    path: Option<PathBuf>,
}

impl Mode for DelegateMode {
    fn run(self, x_env: ClientEnv) -> Result<Client> {
        if self.systemd && self.path.is_some() {
            bail!("Conflicting options: specify either --systemd or --path, not both.")
        };

        let socket = match self.systemd {
            // Safety: by setting --systemd the user guarantees the .socket unit to be a datagram socket
            true => unsafe {
                systemd::socket_activation::listen_fd_simple()
                    .context("Failed to receive a socket from systemd")?
            },
            false => {
                let path = self.path.context("Socket path not specified")?;
                UnixDatagram::bind(&path).context(format!("Cannot bind to socket at {path:?}"))?
            }
        };

        let msg = x_env
            .to_env_diff()
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Set { .. } => Some(entry.to_os_string()),
                Entry::Unset { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(OsStr::new(";"));

        socket
            .send(msg.as_bytes())
            .context("Failed to send environment data")?;

        Ok(Client::None)
    }
}

#[derive(FromArgs)]
#[argh(subcommand)]
#[enum_dispatch(Mode)]
enum ModeSubcommand {
    Direct(DirectMode),
    XinitCompat(XinitCompatMode),
    Delegate(DelegateMode),

    #[cfg(feature = "session")]
    Session(SessionMode),
}

// TODO: merge into envy?
fn env_erase<T: Diff>(x: T) -> EnvBuf {
    EnvBuf::from_entries(x.to_env_diff())
}

#[cfg(feature = "dbus")]
mod resolve_env {
    use super::*;

    #[derive(argh::FromArgValue)]
    pub enum Strategy {
        /// use unix session environment (shell profile)
        Unix,
        /// use systemd environment
        Systemd,
        /// merge systemd and unix environment. unix values take precedence
        Merge,
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
        let mode = &args.env;

        let unix_env = OsEnv::new_view();

        if matches!(mode, Strategy::Unix) {
            return Ok(env_erase(unix_env));
        }

        let session_bus = zbus::Connection::session()
            .await
            .context("Failed to connect to DBus (session bus)")?;

        let systemd_env = systemd::dbus::environment::SystemdEnvironment::open(&session_bus)
            .await
            .context("Cannot resolve systemd environment")?;

        let path = env_path_merge(&systemd_env, &unix_env);

        if matches!(mode, Strategy::Systemd) {
            let mut env = env_erase(systemd_env);
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
        Ok(env_erase(OsEnv::new_view()))
    }
}

#[derive(FromArgs)]
/// Run Xorg like a wayland session
struct Args {
    /// override the path used to exec Xorg
    #[argh(option, default = "DEFAULT_XORG_PATH.into()")]
    xorg_path: PathBuf,

    /// omit XAuthority locking (use at your own risk!)
    #[argh(switch)]
    skip_locks: bool,

    /// use systemd notifications
    #[argh(switch)]
    notify: bool,

    #[cfg(feature = "dbus")]
    #[argh(option)]
    /// environment resolution strategy
    env: resolve_env::Strategy,

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
    let env = resolve_env::resolve_env(&args).await?;

    // TODO: make this non-fatal, fallback to stderr
    journald::init_journald().context("Failed to initialize journald client")?;
    simple_eyre::install()?;

    let context = context::xdg_env(&env).context("Failed to aqquire session context")?;

    // TODO: is this even relevant?
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

    let client = args.mode.run((
        display,
        client_authority,
        window_path,
        // TODO: unset should come from context
        diff::unset::<VtNumber>(),
        diff::unset::<Seat>(),
    ))?;

    if let Some(ref mut notifier) = notifier {
        notifier
            .notify_ready()
            .context("Failed to signal readiness")?;
    }

    // TODO: is there a point in waiting on Xorg? Client should always close if XServer drops, right?
    // ...will systemd reap the zombie as part of session logout?
    client.bind()?;

    if let Some(ref mut notifier) = notifier {
        let _best_effort = notifier.notify_stopping();
    }

    Ok(())
}
