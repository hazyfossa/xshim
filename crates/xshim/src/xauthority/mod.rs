mod encoding;
use binrw::{BinWrite, io::NoSeek};
pub use encoding::*;

mod file;
use eyre::{Context, Result};
use file::*;

use std::path::PathBuf;

use envy::define_env;
use rustix::{
    rand::{GetRandomFlags, getrandom},
    system::uname,
};

use crate::{
    Display,
    utils::{
        private_file::{PrivateFile, SealedPrivateFile},
        runtime_dir::RuntimeDir,
    },
};

define_env!(pub Home(PathBuf) = #raw "HOME");
define_env!(pub ClientAuthorityEnv(PathBuf) = #raw "XAUTHORITY");

fn make_cookie() -> Result<Cookie> {
    let mut cookie_buf = [0u8; Cookie::BYTES_LEN];
    getrandom(&mut cookie_buf, GetRandomFlags::empty()).context("getrandom() failed")?;
    Ok(Cookie::new(cookie_buf))
}

fn get_hostname() -> Hostname {
    uname().nodename().to_bytes().to_vec()
}

fn get_xauthority_path(env: &impl envy::Get) -> Result<PathBuf> {
    env.get::<ClientAuthorityEnv>()
        .map(|v| v.0)
        .or_else(|_| {
            let runtime_dir = RuntimeDir::from_env(env)?;
            eyre::Ok(runtime_dir.join("Xauthority"))
        })
        .or_else(|_| {
            let home = env.get::<Home>()?;
            eyre::Ok(home.join(".Xauthority"))
        })
        .context("Cannot determine an appropriate path from env")
}

// TODO: is there anything we should do when hostname changes?
// Session should stay alive as clients fallback to local
// Are there any side-effects? What breaks?
pub struct XAuthorityManager {
    skip_locks: bool,
    xauthority_path: PathBuf,
    cookie: Cookie,
    hostname: Hostname,
}

impl XAuthorityManager {
    pub fn new(
        skip_locks: bool,
        xauthority_path: &Option<PathBuf>,
        env: &impl envy::Get,
    ) -> Result<Self> {
        let cookie = make_cookie()?;
        let hostname = get_hostname();

        let xauthority_path = xauthority_path
            .clone()
            .unwrap_or(get_xauthority_path(env).context("Failed to get Xauthority path")?);

        Ok(Self {
            skip_locks,
            xauthority_path,
            cookie,
            hostname,
        })
    }

    pub fn setup_server(&self) -> Result<SealedPrivateFile> {
        let file = PrivateFile::new("x-server-authority-data")
            .context("Failed to create a private file via memfd")?;

        let mut writer = NoSeek::new(file);
        Entry::new(&self.cookie, Scope::Any, Target::Server { slot: 0 }).write(&mut writer)?;
        let file = writer.into_inner().seal()?;

        Ok(file)
    }

    pub fn setup_client(&self, display: &Display) -> Result<ClientAuthorityEnv> {
        // TODO: add proper note why we do two entries
        // (legacy apps + hostname changes)

        let authority = [
            Entry::new(
                &self.cookie,
                Scope::Any,
                Target::Client {
                    display_number: display.number(),
                },
            ),
            Entry::new(
                &self.cookie,
                Scope::Local(self.hostname.clone()),
                Target::Client {
                    display_number: display.number(),
                },
            ),
        ];

        let path = &self.xauthority_path;

        let mut xauth_file = if self.skip_locks {
            // Safety: setting `skip_locks` means user explicitly guarantees no other
            // party will interact with Xauthority during setup
            unsafe { AuthorityFile::open_or_create_unlocked(path) }
        } else {
            AuthorityFile::open_or_create(path)
        }
        .context(format!("Failed to create {path:?}"))?;

        // TODO: merge, not overwrite
        xauth_file.set(authority)?;

        Ok(path.clone().into())
    }

    pub fn finalize_into_cookie(self) -> Cookie {
        self.cookie
    }
}
