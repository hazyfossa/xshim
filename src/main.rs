mod environment;
mod runtime_dir;
mod utils;
mod xauthority;

use std::{
    io::{BufRead, BufReader, PipeReader, pipe},
    path::{Path, PathBuf},
    process::{Child, Command},
};

use anyhow::{Context, Result, anyhow, bail};
use facet::Facet;
use figue as fig;

use crate::{
    environment::{Env, EnvironmentParse},
    runtime_dir::RuntimeDirManager,
    utils::fd::{CommandFdCtxExt, FdContext},
    xauthority::XAuthorityManager,
};

static DEFAULT_XORG_PATH: &str = "/usr/lib/Xorg";

environment::define_env!(Seat(String) = parse "XDG_SEAT");
environment::define_env!(pub VtNumber(u8) = "XDG_VTNR");

impl EnvironmentParse<String> for VtNumber {
    fn env_serialize(self) -> String {
        self.0.to_string()
    }

    fn env_deserialize(raw: String) -> anyhow::Result<Self> {
        Ok(Self(raw.parse()?))
    }
}

environment::define_env!(Display(u8) = "DISPLAY");

impl Display {
    fn number(&self) -> u8 {
        self.0
    }
}

impl EnvironmentParse<String> for Display {
    fn env_serialize(self) -> String {
        format!(":{}", self.0).into()
    }

    fn env_deserialize(value: String) -> Result<Self> {
        Ok(Self(
            value
                .strip_prefix(":")
                .ok_or(anyhow!("display should start with :"))?
                .parse()?,
        ))
    }
}

struct DisplayReceiver(PipeReader);

impl DisplayReceiver {
    fn setup<'a>(fd_ctx: &mut FdContext, command: &'a mut Command) -> Result<Self> {
        let (display_rx, display_tx) = pipe().context("Failed to open pipe for display fd")?;

        let display_tx_passed = fd_ctx.pass(display_tx.into())?;

        command.args(["-displayfd", &display_tx_passed.num().to_string()]);

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

        Ok(Display(display_number))
    }
}

fn spawn_server(
    path: &Path,
    authority: PathBuf,
    vt: VtNumber,
    seat: Option<Seat>,
) -> Result<(DisplayReceiver, Child)> {
    let mut fd_ctx = FdContext::new(3..5);

    let mut command = Command::new(path);

    if let Some(seat) = seat {
        command.args(["-seat".into(), seat.0]);
    }

    command
        .arg(format!("vt{}", vt.0.to_string()))
        .args(["-auth".into(), authority])
        .args(["-nolisten", "tcp"])
        .args(["-background", "none", "-noreset", "-keeptty", "-novtswitch"])
        .args(["-verbose", "3", "-logfile", "/dev/null"])
        .envs([("XORG_RUN_AS_USER_OK", "1")]); // TODO: relevant?

    let display_rx = DisplayReceiver::setup(&mut fd_ctx, &mut command)?;
    command.with_fd_context(fd_ctx);

    // TODO: proxy logs
    let child = command.spawn()?;

    Ok((display_rx, child))
}

#[derive(Facet)]
struct Args {
    #[facet(fig::positional)]
    client: Option<PathBuf>,
    #[facet(default = DEFAULT_XORG_PATH)]
    xorg_path: PathBuf,
    #[facet(default = false)]
    skip_locks: bool,
}

fn main() -> Result<()> {
    let env = environment::EnvOs::new_view();
    let args: Args = fig::from_std_args().unwrap();

    // TODO: add an unsafe option to try and determine one anyway
    let vt = env.get::<VtNumber>().context("Cannot find a VT number allocated for current session. Are you running this from a correct place?")?;
    let seat = env.get::<Seat>();

    // TODO: warn on seat error

    let rt_dir_manager =
        RuntimeDirManager::from_env(&env).context("Cannot setup runtime dir manager")?;

    let runtime_dir = rt_dir_manager
        .create(&format!("xshim-{}", vt.0))
        .context("Cannot create runtime directory")?;

    let authority_manager = XAuthorityManager::new(runtime_dir, args.skip_locks)
        .context("Cannot setup XAuthority manager")?;

    let server_authority = authority_manager
        .setup_server()
        .context("Failed to define server authority")?;

    let (future_display, xorg_child) =
        spawn_server(&args.xorg_path, server_authority, vt, seat.ok())
            .context("Failed to spawn Xorg")?;

    let display = future_display.blocking_wait()?;

    let client_authority = authority_manager
        .setup_client(&display)
        .context("failed to define client authority")?;

    // TODO: spawn client

    Ok(())
}
