use eyre::{Context, Result};
use rustix::rand::{GetRandomFlags, getrandom};

use crate::{
    env_definitions::{Seat, VtNumber},
    utils::warn::WarnExt,
};

pub struct SessionContext {
    pub seat: Option<Seat>,
    pub vt_number: Option<VtNumber>,
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

// let Xorg handle everything
pub fn none() -> SessionContext {
    SessionContext {
        seat: None,
        vt_number: None,
    }
}

pub fn alloc_vt() -> Result<SessionContext> {
    todo!()
}

pub fn xdg_env(env: &impl envy::Get) -> Result<SessionContext> {
    let vt_number = env.get::<VtNumber>().context("Cannot find a VT number allocated for current session. Are you running this from a correct place?")?;

    let seat = env
        .get::<Seat>()
        .context("Cannot find a seat for the current session")
        .warn();

    Ok(SessionContext {
        seat,
        vt_number: Some(vt_number),
    })
}

pub fn logind() -> Result<SessionContext> {
    todo!()
}
