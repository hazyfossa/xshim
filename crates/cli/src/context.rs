use argh::FromArgValue;
use envy::{EnvVariable, Get, OsEnv, Set, container::EnvBuf, define_env};
use eyre::{Context, Result};
use freedesktop_session_parser::SessionKind;

use crate::{Args, utils::warn::WarnExt, warn};

define_env!(pub Seat(String) = "XDG_SEAT");
define_env!(pub VtNumber(u32) = "XDG_VTNR");

#[derive(Default)]
pub struct SessionContext {
    pub seat: Option<Seat>,
    pub vt_number: Option<VtNumber>,
    pub env_diff: Option<EnvBuf>,
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
