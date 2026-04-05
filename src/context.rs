use argh::FromArgValue;
use envy::{EnvVariable, Get, OsEnv, Set, container::EnvBuf};
use eyre::{Context, Result};
use freedesktop_session_parser::SessionKind;
use rustix::rand::{GetRandomFlags, getrandom};

use crate::{
    Args,
    env_definitions::{Seat, VtNumber},
    utils::warn::WarnExt,
    warn,
};

#[derive(Default)]
pub struct SessionContext {
    pub seat: Option<Seat>,
    pub vt_number: Option<VtNumber>,
    pub env_diff: Option<EnvBuf>,
}

impl SessionContext {
    pub fn unique_id(&self) -> Result<String> {
        let mut parts = Vec::new();

        if let Some(seat) = &self.seat {
            parts.push(seat.to_string());
        };

        if let Some(vt) = &self.vt_number {
            parts.push(vt.to_string());
        }

        if parts.is_empty() {
            let mut random = [0u8; 4];

            getrandom(&mut random, GetRandomFlags::INSECURE)
                .context("Failed to fetch randomness for a session identifier")?;

            let id = u32::from_ne_bytes(random);
            return Ok(id.to_string());
        }

        Ok(parts.join("-").to_string())
    }
}

#[derive(FromArgValue, Clone)]
pub enum ContextMode {
    // Let Xorg handle everything
    None,
    // Like None, but pre-allocate a VT
    VtAlloc,
    // Use XDG environment variables
    Xdg,
}

impl Default for ContextMode {
    fn default() -> Self {
        Self::Xdg
    }
}

fn alloc_vt() -> Result<VtNumber> {
    todo!()
}

// TODO: seatd support?
// TODO: set XDG type?

fn xdg() -> Result<SessionContext> {
    // We use the system env here regardless of user setting
    // for session env. This is not a mistake.
    //
    // The PAM stack will provide appropriate variables in
    // unix env, not systemd env.
    let mut env = OsEnv::new_view();

    let vt_number = env.get::<VtNumber>().context(
        "Cannot find a VT number allocated for current session. 
Are you running this from a correct place?",
    )?;

    let seat = env
        .get::<Seat>()
        .context("Cannot find a seat for the current session")
        .warn();

    match env.get::<SessionKind>() {
        Ok(SessionKind::X11) => (),
        Ok(other) => {
            warn!(
                "{} variable is incorrect: expected '{}', got '{}'.
                Logind will register this session as wrong type.
                You should set this variable via your session manager.
                ",
                SessionKind::KEY,
                SessionKind::X11,
                other
            );
        }

        Err(envy::Error::NoneError { .. }) => env.set(SessionKind::X11),
        Err(other) => {
            warn!("{other}");
        }
    }

    Ok(SessionContext {
        seat,
        vt_number: Some(vt_number),
        env_diff: Some(EnvBuf::from_diff(env)),
    })
}

pub async fn aqquire(args: &Args) -> Result<SessionContext> {
    Ok(match args.context.clone().unwrap_or_default() {
        ContextMode::None => SessionContext::default(),

        ContextMode::VtAlloc => SessionContext {
            vt_number: Some(alloc_vt()?),
            ..Default::default()
        },

        ContextMode::Xdg => xdg()?,
    })
}
