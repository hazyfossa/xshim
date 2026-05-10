use std::{
    io::{BufRead, BufReader, PipeReader, pipe},
    os::fd::AsRawFd,
    path::PathBuf,
    process::Command,
};

use envy::{Get, OsEnv, container::EnvBuf, define_env};
use eyre::{Context, Result, bail};

use crate::{
    utils::{
        fd::{CommandFdExt, FdContext, SimpleFdContext},
        private_file::SealedPrivateFile,
        subprocess::CleanupExt,
    },
    xauthority::{ClientAuthorityEnv, XAuthorityManager},
};

mod utils;
pub use utils::subprocess;

mod xauthority;

#[cfg(feature = "client")]
pub use x11rb::rust_connection::RustConnection as XConnection;

// You may want to change this if you're making a package
const DEFAULT_XORG_PATH: &str = "/usr/lib/Xorg";

define_env!(pub Seat(String) = "XDG_SEAT");
define_env!(pub VtNumber(u32) = "XDG_VTNR");
define_env!(pub Display(u16) = "DISPLAY");
define_env!(pub WindowPath(String) = "WINDOWPATH");

impl WindowPath {
    fn previous_plus_vt(env: &impl envy::Get, vt: &VtNumber) -> Self {
        let previous = env.get::<Self>();
        Self(match previous {
            Ok(path) => format!("{}:{}", path.0, vt.0),
            Err(_) => vt.to_string(),
        })
    }
}

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
            bail!("Internal Xorg error")
        }

        let display_number = display_buf
            .trim_end()
            .parse::<u16>()
            .context("Xorg provided invalid display number")?;

        Ok(Display::from(display_number))
    }
}

fn prepare_xorg(
    path: PathBuf,
    vt: Option<VtNumber>,
    seat: Option<Seat>,
    server_authority: SealedPrivateFile,
    extra_args: Vec<String>,
) -> Result<(DisplayReceiver, Command)> {
    let mut fd_context = FdContext::new(2);

    let server_authority = fd_context.pass(server_authority.into_inner())?;

    let mut command = Command::new(path);

    if let Some(seat) = seat {
        command.args(["-seat", &seat]);
    }

    if let Some(vt) = vt {
        command.arg(format!("vt{}", vt.0)).arg("-novtswitch");
    }

    command
        .args(["-auth".into(), server_authority.path()])
        .args(["-nolisten", "tcp"])
        .args(["-background", "none", "-noreset", "-keeptty"])
        .args(["-verbose", "3", "-logfile", "/dev/null"])
        .args(extra_args)
        .envs([("XORG_RUN_AS_USER_OK", "1")]);

    let display_rx = DisplayReceiver::setup(&mut fd_context, &mut command)?;

    command.with_fd_context(fd_context).with_cleanup();

    Ok((display_rx, command))
}

#[cfg(any(feature = "client", feature = "xrdb"))]
fn xorg_connection(
    display: &Display,
    cookie: &xauthority::Cookie,
) -> Result<x11rb::rust_connection::RustConnection> {
    use eyre::OptionExt;
    use x11rb::reexports::x11rb_protocol::parse_display::ParsedDisplay;
    use x11rb::rust_connection::DefaultStream;

    let display = ParsedDisplay {
        host: "".into(), // Use hostname from XAuthorityManager?
        protocol: None,
        display: **display,
        screen: 0,
    };

    let conn = display.connect_instruction().find_map(|c| {
        let (stream, _) = DefaultStream::connect(&c).ok()?;
        XConnection::connect_to_stream_with_auth_info(
            stream,
            0,
            xauthority::Cookie::AUTH_NAME.into(),
            cookie.raw_data(),
        )
        .ok()
    });

    conn.ok_or_eyre("Failed to connect to Xorg")
}

#[derive(Default)]
#[cfg_attr(feature = "settings", derive(bon::Builder))]
pub struct Settings {
    /// Path to Xorg binary
    path: Option<PathBuf>,

    /// Override current environment
    env: Option<EnvBuf>,

    /// VT number to use.
    /// If set to None, it will be determined by Xorg.
    vt: Option<VtNumber>,

    /// Login seat to use.
    /// If set to None, Xorg will operate without a seat.
    seat: Option<Seat>,

    /// Extra arguments to pass to Xorg
    extra_args: Vec<String>,

    /// Where to place the XAuthority file
    xauthority_path: Option<PathBuf>,

    /// Do not use locks when dealing with Xauthority.
    /// Marginally improves performance.
    ///
    /// Safety:
    /// Only set if sure no other process will interact with Xauthority while in setup.
    /// Usage with `xauthority_path` unset is generally unsafe.
    unsafe_skip_locks: Option<bool>,

    /// Override paths used for Xresources loading.
    #[cfg(feature = "xrdb")]
    resources: Option<Vec<PathBuf>>,
}

pub struct XShim {
    pub xorg_child: std::process::Child,
    pub client_env: (Display, ClientAuthorityEnv, Option<WindowPath>),
    #[cfg(feature = "client")]
    pub connection: XConnection,
}

/// Returns (xorg_child, client_env)
/// Will block the current thread until Xorg provides a display
///
/// Should be called from the context of the session user, *not* the root user
/// (Xorg as root is discouraged)
// TODO: optionally switch user on spawn
pub fn setup_xorg_with_settings(mut settings: Settings) -> Result<XShim> {
    let env = settings.env.take().unwrap_or(OsEnv::new_view().into());

    let vt = settings.vt.or(env.get().ok());
    let seat = settings.seat.or(env.get().ok());

    let window_path = vt.as_ref().map(|vt| WindowPath::previous_plus_vt(&env, vt));

    let authority_manager = XAuthorityManager::new(
        settings.unsafe_skip_locks.unwrap_or(false),
        &settings.xauthority_path,
        &env,
    )
    .context("Cannot setup XAuthority manager")?;

    let server_authority = authority_manager
        .setup_server()
        .context("Failed to define server authority")?;

    let (future_display, mut xorg_command) = prepare_xorg(
        settings.path.unwrap_or(DEFAULT_XORG_PATH.into()),
        vt,
        seat,
        server_authority,
        settings.extra_args,
    )?;

    let xorg_child = xorg_command.spawn().context("Failed to spawn Xorg")?;

    let display = future_display.blocking_wait()?;

    let client_authority = authority_manager
        .setup_client(&display)
        .context("Failed to define client authority")?;

    let cookie = authority_manager.finalize_into_cookie();

    #[cfg(any(feature = "client", feature = "xrdb"))]
    let connection = xorg_connection(&display, &cookie)?;

    // TODO: xrdb

    drop(cookie);

    Ok(XShim {
        xorg_child,
        client_env: (display, client_authority, window_path),

        // returns the connection if "client" feature is toggled, drops otherwise
        #[cfg(feature = "client")]
        connection,
    })
}

pub fn setup_xorg() -> Result<XShim> {
    setup_xorg_with_settings(Settings::default())
}
