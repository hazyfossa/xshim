use std::{
    fs::{self, DirBuilder, remove_dir_all},
    ops::Deref,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};

use crate::frame::environment::{Env, define_env};

pub struct RuntimeDir {
    pub path: PathBuf,
}

impl Deref for RuntimeDir {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        let _ = remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
pub struct RuntimeDirManager {
    path: PathBuf,
}

define_env!(pub RuntimeDirEnv(PathBuf) = parse "XDG_RUNTIME_DIR");

impl RuntimeDirManager {
    pub fn from_env(env: &impl Env) -> Result<Self> {
        let path = env
            .get::<RuntimeDirEnv>()
            .context("Environment does not provide a runtime directory")?
            .0;

        let permissions = fs::metadata(&path)
            .context("Cannot query runtime dir metadata. Does it exist?")?
            .permissions()
            .mode();

        if permissions & 0o077 != 0 {
            bail!("Runtime directory is insecure: expecting permissions `077`, got {permissions}")
        };

        Ok(Self { path })
    }

    pub fn create(&self, name: &str) -> Result<RuntimeDir> {
        let directory = self.path.join(name);

        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .context(format!("cannot create path: {directory:?}"))?;

        Ok(RuntimeDir { path: directory })
    }
}
