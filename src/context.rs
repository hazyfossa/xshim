use argh::FromArgValue;
use eyre::{Context, Result};
use rustix::rand::{GetRandomFlags, getrandom};

use crate::{
    Args,
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

#[derive(FromArgValue, Clone)]
pub enum ContextMode {
    // Let Xorg handle everything
    None,
    // Like None, but pre-allocate a VT
    VtAlloc,
    // Use XDG environment variables
    XDG,
    // Become a full logind session leader
    #[cfg(feature = "dbus")]
    Logind,
}

#[cfg(feature = "dbus")]
impl Default for ContextMode {
    fn default() -> Self {
        Self::Logind
    }
}

#[cfg(not(feature = "dbus"))]
impl Default for ContextMode {
    fn default() -> Self {
        Self::XDG
    }
}

fn alloc_vt() -> Result<VtNumber> {
    todo!()
}

// TODO: seatd support?

#[cfg(feature = "dbus")]
async fn logind_takeover() -> Result<SessionContext> {
    use crate::systemd::dbus::logind::{LoginManagerProxy, SessionProxy};

    let dbus = zbus::Connection::system()
        .await
        .context("Failed to connect to DBus (system bus)")?;

    let logind = LoginManagerProxy::new(&dbus)
        .await
        .context("Failed to connect to logind")?;

    let pid_self = rustix::process::getpid().as_raw_pid();

    let session_path = logind
        .get_session_by_pid(
            pid_self
                .try_into()
                .context("Unusual PID of current process, cannot pass to logind")?,
        )
        .await
        .context("Logind query failed: is xshim running in an existing session?")?;

    let session = SessionProxy::builder(&dbus)
        .path(session_path)
        .context("Logind provided invalid dbus path")?
        .build()
        .await
        .context("Failed to interact with session over DBus")?;

    // what does `force` mean here?
    session
        .take_control(false)
        .await
        .context("Failed to take control of the session")?;

    let (seat, _dbus_path) = session.seat().await.context("Failed to query seat")?;
    let vt = session.vtnr().await.context("Failed to query VT number")?;

    session
        .set_type("x11")
        .await
        .context("Failed to set session type")?;

    // TODO:
    // session.set_display(display)

    Ok(SessionContext {
        seat: Some(seat.into()),
        vt_number: Some(vt.into()),
    })
}

pub async fn aqquire(args: &Args, env: &impl envy::Get) -> Result<SessionContext> {
    Ok(match args.context.clone().unwrap_or_default() {
        ContextMode::None => SessionContext {
            seat: None,
            vt_number: None,
        },
        ContextMode::VtAlloc => SessionContext {
            seat: None,
            vt_number: Some(alloc_vt()?),
        },
        ContextMode::XDG => {
            let vt_number = env.get::<VtNumber>().context(
                "Cannot find a VT number allocated for current session. 
                    Are you running this from a correct place?",
            )?;

            let seat = env
                .get::<Seat>()
                .context("Cannot find a seat for the current session")
                .warn();

            SessionContext {
                seat,
                vt_number: Some(vt_number),
            }
        }
        #[cfg(feature = "dbus")]
        ContextMode::Logind => logind_takeover().await?,
    })
}
