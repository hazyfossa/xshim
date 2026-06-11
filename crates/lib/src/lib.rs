use std::{
    ffi::OsString,
    io::{BufRead, BufReader, PipeReader, pipe},
    os::fd::AsRawFd,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

use envy::{Get, OsEnv, container::EnvBuf, define_env};
use eyre::{Context, Result, bail};

use crate::{
    utils::{
        fd::{CommandFdExt, FdContext},
        hostname,
        private_file::SealedPrivateFile,
        subprocess::CleanupExt,
    },
    xauthority::{ClientAuthority, ClientAuthoritySettings},
};

mod utils;
mod xauthority;

#[cfg(feature = "client")]
pub use x11rb::rust_connection::RustConnection as XConnection;

// You may want to change this if you're making a package
const DEFAULT_XORG_PATH: &str = "/usr/lib/Xorg";

define_env!(pub Seat(String) = "XDG_SEAT");
define_env!(pub VtNumber(u32) = "XDG_VTNR");
define_env!(pub Display(u16) = "DISPLAY");

struct DisplayReceiver(PipeReader);

impl DisplayReceiver {
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
    authority: SealedPrivateFile,
    log_level: u8,
    extra_args: Option<Vec<String>>,
) -> Result<(DisplayReceiver, Command)> {
    let mut fd_ctx = FdContext::new();
    let mut command = Command::new(path);

    // Defaults
    command
        .args(["-background", "none", "-noreset", "-keeptty"])
        .args(["-nolisten", "tcp"])
        .envs([("XORG_RUN_AS_USER_OK", "1")]);

    // vt/seat

    if let Some(vt) = vt {
        command.arg(format!("vt{}", vt.0)).arg("-novtswitch");
    };

    if let Some(seat) = seat {
        command.args(["-seat", &seat]);
    };

    // authority

    let passed_authority = fd_ctx.pass(authority.into_inner());
    command.args(["-auth".into(), passed_authority.path()]);

    // logging

    command
        .stdout(Stdio::null())
        .arg("-logfile /dev/null")
        .args(["-verbose", &log_level.to_string()]);

    // display receiver

    let (rx, tx) = pipe().context("Failed to open pipe for display fd")?;
    let display_fd = fd_ctx.pass(tx.into());
    let display_rx = DisplayReceiver(rx);
    command.args(["-displayfd", &display_fd.as_raw_fd().to_string()]);

    // other

    if let Some(extra_args) = extra_args {
        command.args(extra_args);
    }

    command.with_fd_context(fd_ctx).with_cleanup();

    Ok((display_rx, command))
}

#[cfg(feature = "client")]
fn connect_xorg(
    display: &Display,
    cookie: &xauthority::Cookie,
) -> Result<x11rb::rust_connection::RustConnection> {
    use eyre::OptionExt;
    use x11rb::reexports::x11rb_protocol::parse_display::ParsedDisplay;
    use x11rb::rust_connection::DefaultStream;

    let display = ParsedDisplay {
        host: "".into(),
        protocol: None,
        display: **display,
        screen: 0,
    };

    let conn = display.connect_instruction().find_map(|c| {
        let (stream, _) = DefaultStream::connect(&c).ok()?;
        x11rb::rust_connection::RustConnection::connect_to_stream_with_auth_info(
            stream,
            0,
            xauthority::Cookie::AUTH_NAME.into(),
            cookie.0.to_vec(),
        )
        .ok()
    });

    conn.ok_or_eyre("Failed to connect to Xorg")
}

pub struct PendingSession {
    display_receiver: DisplayReceiver,
    cookie: xauthority::Cookie,
    client_authority_settings: xauthority::ClientAuthoritySettings,
}

impl PendingSession {
    /// This function will block the current thread until Xorg provides a display
    /// This also means it will deadlock if `xorg_command` is not spawned
    pub fn wait_for_display(self) -> Result<XSession> {
        let cookie = self.cookie;
        let display = self.display_receiver.blocking_wait()?;

        let client_authority =
            xauthority::setup_client(self.client_authority_settings, &cookie, &display)
                .context("Failed to setup client authority")?;

        Ok(XSession {
            display,
            client_authority,
            #[cfg(feature = "client")]
            cookie,
        })
    }
}

pub struct XShim {
    pub xorg_command: Command,
    pub pending_session: PendingSession,
}

impl XShim {
    /// This function will block the current thread until Xorg provides a display
    ///
    /// If you want more control, consider spawning `xorg_command` manually
    /// and using `self.pending_session.wait_for_display` instead
    pub fn spawn(mut self) -> Result<(Child, XSession)> {
        let child = self.xorg_command.spawn().context("Failed to spawn Xorg")?;
        Ok((child, self.pending_session.wait_for_display()?))
    }
}

pub struct XSession {
    display: Display,
    client_authority: ClientAuthority,
    #[cfg(feature = "client")]
    cookie: xauthority::Cookie,
}

impl XSession {
    #[cfg(feature = "client")]
    pub fn connect(&self) -> Result<XConnection> {
        connect_xorg(&self.display, &self.cookie).context("Failed to connect to Xorg")
    }

    pub fn client_env(self) -> (Display, ClientAuthority) {
        (self.display, self.client_authority)
    }
}

#[derive(bon::Builder)]
pub struct Settings {
    /// Path to Xorg binary
    #[builder(default = DEFAULT_XORG_PATH.into())]
    xorg_path: PathBuf,

    /// Override current environment
    env: Option<EnvBuf>,

    /// Override current hostname
    hostname: Option<OsString>,

    /// VT number to use.
    /// If set to None, it will be queried from environment
    /// if env query fails, an appropriate vt will be determined by Xorg
    vt: Option<VtNumber>,

    /// Login seat to use.
    /// If set to None, it will be queried from environment
    /// if env query fails, Xorg will operate without a seat
    seat: Option<Seat>,

    /// Xorg log (verbosity) level
    #[builder(default = 3)]
    log_level: u8,

    /// Extra arguments to pass to Xorg
    extra_args: Option<Vec<String>>,

    /// Where to place the XAuthority file
    xauthority_path: Option<PathBuf>,

    /// Do not use locks when dealing with Xauthority.
    /// Marginally improves performance.
    ///
    /// Safety:
    /// Only set if sure no other process will interact with Xauthority while in setup.
    /// Usage with `xauthority_path` unset is generally unsafe.
    #[builder(default = false)]
    unsafe_skip_xauth_locks: bool,
}

/// Returns a handle to a prepared instance.
/// Think of it as a better `Command::new("Xorg")`
///
/// You can resolve the handle into a running Xorg process via `.spawn()`
/// Or spawn `.xorg_command` manually, then call `.wait_for_display()`
pub fn xorg_new(mut settings: Settings) -> Result<XShim> {
    let env = settings.env.take().unwrap_or(OsEnv::new_view().into());

    let vt = settings.vt.or(env.get().ok());
    let seat = settings.seat.or(env.get().ok());
    let hostname = settings.hostname.unwrap_or(hostname::current());

    let xauthority_path = settings
        .xauthority_path
        .unwrap_or(xauthority::get_xauthority_path(&env)?);

    let (server_authority, cookie) =
        xauthority::setup_server().context("Failed to define server authority")?;

    let client_authority_settings = ClientAuthoritySettings {
        xauthority_path,
        hostname,
        skip_locks: settings.unsafe_skip_xauth_locks,
    };

    let (display_receiver, xorg) = prepare_xorg(
        settings.xorg_path,
        vt,
        seat,
        server_authority,
        settings.log_level,
        settings.extra_args,
    )?;

    Ok(XShim {
        xorg_command: xorg,
        pending_session: PendingSession {
            display_receiver,
            cookie,
            client_authority_settings,
        },
    })
}

/// See `xorg_new_with_settings` for documentation
pub fn xorg_new_default() -> Result<XShim> {
    xorg_new(Settings::builder().build())
}
