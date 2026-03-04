mod environment;
mod runtime_dir;
mod utils;
mod xauthority;

use std::{
    io::{BufRead, BufReader, PipeReader, pipe},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    environment::EnvironmentParse,
    utils::fd::{CommandFdCtxExt, FdContext},
};

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

    async fn display(self) -> Result<Display> {
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
) -> Result<DisplayReceiver> {
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

    // TODO: spawn and proxy logs

    Ok(display_rx)
}

fn main() {
    println!("Hello, world!");
}
