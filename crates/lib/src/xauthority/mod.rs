mod encoding;
use binrw::{BinWrite, io::NoSeek};
pub use encoding::*;

mod file;
use eyre::{Context, Result};
use file::*;

use std::path::PathBuf;

use envy::define_env;
use rustix::rand::{GetRandomFlags, getrandom};

use crate::{
    Display,
    utils::{
        hostname::Hostname,
        private_file::{PrivateFile, SealedPrivateFile},
        runtime_dir::RuntimeDir,
    },
};

define_env!(pub Home(PathBuf) = #raw "HOME");
define_env!(pub ClientAuthority(PathBuf) = #raw "XAUTHORITY");

fn make_cookie() -> Result<Cookie> {
    let mut cookie_buf = [0u8; Cookie::BYTES_LEN];
    getrandom(&mut cookie_buf, GetRandomFlags::empty()).context("getrandom() failed")?;
    Ok(Cookie(cookie_buf))
}

pub fn get_xauthority_path(env: &impl envy::Get) -> Result<PathBuf> {
    env.get::<ClientAuthority>()
        .map(|v| v.0)
        .or_else(|_| {
            let runtime_dir = RuntimeDir::from_env(env)?;
            eyre::Ok(runtime_dir.join("Xauthority"))
        })
        .or_else(|_| {
            let home = env.get::<Home>()?;
            eyre::Ok(home.join(".Xauthority"))
        })
        .context("Cannot determine an appropriate Xauthority path")
}

pub fn setup_server() -> Result<(SealedPrivateFile, Cookie)> {
    let cookie = make_cookie().context("Failed to make cookie")?;

    let file = PrivateFile::new("x-server-authority-data")
        .context("Failed to create a private file via memfd")?;

    let mut writer = NoSeek::new(file);

    Entry::builder()
        .cookie(cookie.clone())
        .scope(Scope::Any)
        .target(0)
        .build()
        .write(&mut writer)?;

    let file = writer
        .into_inner()
        .seal()
        .context("Failed to seal the private file")?;

    Ok((file, cookie))
}

pub struct ClientAuthoritySettings {
    pub xauthority_path: PathBuf,
    pub hostname: Hostname,
    pub skip_locks: bool,
}

pub fn setup_client(
    settings: ClientAuthoritySettings,
    cookie: &Cookie,
    display: &Display,
) -> Result<ClientAuthority> {
    // The local entry is for applications which may not support wildcard authority
    // The wildcard entry exists so client do not break on hostname change

    let authority = [
        Entry::builder()
            .cookie(cookie.clone())
            .scope(Scope::Any)
            .target(**display)
            .build(),
        Entry::builder()
            .cookie(cookie.clone())
            .scope(Scope::Local(settings.hostname.clone()))
            .target(**display)
            .build(),
    ];

    let path = settings.xauthority_path;

    let mut xauth_file = if settings.skip_locks {
        // Safety: setting `skip_locks` means user explicitly guarantees no other
        // party will interact with Xauthority during setup
        unsafe { AuthorityFile::open_or_create_unlocked(&path) }
    } else {
        AuthorityFile::open_or_create(&path)
    }
    .context(format!("Failed to create {path:?}"))?;

    // TODO: merge, not overwrite
    xauth_file.set(authority)?;

    Ok(path.into())
}
