mod encoding;
use encoding::*;

mod file;
use file::*;

use std::{
    io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rustix::{
    rand::{GetRandomFlags, getrandom},
    system::uname,
};

use crate::{Display, environment::define_env, runtime_dir::RuntimeDir};

define_env!(pub ClientAuthorityEnv(PathBuf) = parse "XAUTHORITY");

fn make_cookie() -> Result<Cookie> {
    let mut cookie_buf = [0u8; Cookie::BYTES_LEN];
    getrandom(&mut cookie_buf, GetRandomFlags::empty()).context("getrandom() failed")?;
    Ok(Cookie::new(cookie_buf))
}

fn get_hostname() -> Hostname {
    uname().nodename().to_bytes().to_vec()
}

// TODO: is there anything we should do when hostname changes?
// Session should stay alive as clients fallback to local
// Are there any side-effects? What breaks?
pub struct XAuthorityManager {
    skip_locks: bool,
    runtime_dir: RuntimeDir,
    cookie: Cookie,
    hostname: Hostname,
}

impl XAuthorityManager {
    pub fn new(runtime_dir: RuntimeDir, skip_locks: bool) -> Result<Self> {
        let cookie = make_cookie()?;
        let hostname = get_hostname();

        Ok(Self {
            skip_locks,
            runtime_dir,
            cookie,
            hostname,
        })
    }

    fn create_auth_file(&self, path: &Path) -> io::Result<AuthorityFile> {
        if self.skip_locks {
            // Safety: setting lock=false means user explicitly guarantees no other
            // party will interact with runtime dir on setup
            // TODO: maybe propagate safety of lock option better
            unsafe { AuthorityFile::create_unlocked(path) }
        } else {
            AuthorityFile::create(path)
        }
    }

    pub fn setup_server(&self) -> Result<PathBuf> {
        let authority = [Entry::new(
            &self.cookie,
            Scope::Any,
            Target::Server { slot: 0 },
        )];

        let path = self.runtime_dir.join("server-authority");

        let mut xauth_file = self
            .create_auth_file(&path)
            .context(format!("Failed to create {path:?}"))?;

        xauth_file.set(authority)?;

        Ok(path)
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

        let path = self.runtime_dir.join("client-authority");

        let mut xauth_file = self
            .create_auth_file(&path)
            .context(format!("Failed to create {path:?}"))?;

        xauth_file.set(authority)?;

        Ok(ClientAuthorityEnv(path.into()))
    }

    pub fn finish(self) -> RuntimeDir {
        self.runtime_dir
    }
}
