mod env_definitions;
mod error;
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
    Env,
    container::OsEnv,
    define_env,
    diff::{Diff, EnvVecExt},
};

use crate::{
    env_definitions::*,
    error::*,
    runtime_dir::RuntimeDirManager,
    systemd::{journald, notify::Notifier},
    utils::{
        fd::{CommandFdExt, FdContext, SimpleFdContext},
        path::EnsureExistsExt,
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
    let child = spawn_with_cleanup(command).ctx("Failed to spawn Xorg")?;

    Ok((display_rx, child))
}

type ClientEnv = (Display, ClientAuthorityEnv, WindowPath);

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
                .ctx("Failed to spawn client subprocess")?
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
        command.envs(x_env.to_env_diff());

        Ok(Client::Command(command))
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
    fn run(self, x_env: ClientEnv) -> Result<Client> {
        todo!("session mode not implemented")
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
                .ctx("cannot find the user home directory")?
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
        command.envs(x_env.clone().to_env_diff());

        let client_process = match spawn_with_cleanup(command) {
            Ok(process) => Ok(process),
            // retry with shell
            Err(e) if e.kind() != ErrorKind::PermissionDenied => {
                let mut with_shell = Command::new("/bin/sh");
                with_shell.arg(client_path).envs(x_env.to_env_diff());

                spawn_with_cleanup(with_shell)
            }
            Err(other) => Err(other),
        };

        Ok(Client::Process(client_process))
    }
}

#[derive(FromArgs)]
#[argh(subcommand, name = "xorg-delegate")]
/// Delegate client lifecycle. Called by a cooperative session manager.
struct DelegateMode {
    #[argh(switch)]
    /// use systemd socket activation
    systemd: bool,
    #[argh(option)]
    /// use socket on path
    path: Option<PathBuf>,
}

impl Mode for DelegateMode {
    fn run(self, x_env: ClientEnv) -> Result<Client> {
        if self.systemd && self.path.is_some() {
            whatever!("Conflicting options: specify either --systemd or --path, not both.")
        };

        let socket = match self.systemd {
            // Safety: by setting --systemd the user guarantees the .socket unit to be a datagram socket
            true => unsafe {
                systemd::socket_activation::listen_fd_simple()
                    .ctx("Failed to receive a socket from systemd")?
            },
            false => {
                let path = self.path.ctx("Socket path not specified")?;
                UnixDatagram::bind(&path).ctx(format!("Cannot bind to socket at {path:?}"))?
            }
        };

        socket
            .send(x_env.to_vec().join(OsStr::new(";")).as_bytes())
            .ctx("Failed to send environment data")?;

        Ok(Client::None)
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

fn main() -> Result<()> {
    let env = OsEnv::new_view();
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

    let client = args.mode.run((display, client_authority, window_path))?;

    if let Some(ref mut notifier) = notifier {
        notifier.notify_ready().ctx("Failed to signal readiness")?;
    }

    // TODO: is there a point in waiting on Xorg? Client should always close if XServer drops, right?
    // ...will systemd reap the zombie as part of session logout?
    client.bind()?;

    if let Some(ref mut notifier) = notifier {
        let _best_effort = notifier.notify_stopping();
    }

    Ok(())
}
