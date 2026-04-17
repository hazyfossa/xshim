use std::collections::HashSet;

use envy::{OsEnv, Set, container::EnvBuf, define_env, parse::EnvironmentParse};
use eyre::{Context, Result};

use crate::{Args, systemd::dbus::environment::SystemdEnvironment};

#[derive(argh::FromArgValue, Clone)]
pub enum Strategy {
    /// use unix session environment (shell profile)
    Unix,
    /// use systemd environment
    Systemd,
    /// merge systemd and unix environment. unix values take precedence
    Merge,
}

impl Default for Strategy {
    fn default() -> Self {
        Self::Systemd
    }
}

define_env!(pub PathEnv(Vec<String>) = #custom "PATH");

impl EnvironmentParse<String> for PathEnv {
    type Error = std::convert::Infallible;

    fn env_serialize(self) -> String {
        self.0.join(":")
    }

    fn env_deserialize(value: String) -> Result<Self, Self::Error> {
        let values = value.split(':').map(|s| s.to_string()).collect();
        Ok(Self(values))
    }
}

impl std::ops::Add for PathEnv {
    type Output = PathEnv;

    fn add(self, rhs: Self) -> Self::Output {
        let set: HashSet<String> = self.0.into_iter().chain(rhs.0.into_iter()).collect();
        PathEnv(set.into_iter().collect())
    }
}

fn env_path_merge(primary: &impl envy::Get, secondary: &impl envy::Get) -> Option<PathEnv> {
    let a = primary.get::<PathEnv>().ok();
    let b = secondary.get::<PathEnv>().ok();

    match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        (a, None) => a,
        (None, b) => b,
    }
}

pub async fn resolve_env(args: &Args) -> Result<EnvBuf> {
    let mode = &args.env.clone().unwrap_or_default();

    let unix_env = OsEnv::new_view();

    if matches!(mode, Strategy::Unix) {
        return Ok(EnvBuf::from_diff(unix_env));
    }

    let session_bus = zbus::Connection::session()
        .await
        .context("Failed to connect to DBus (session bus)")?;

    let systemd_env = SystemdEnvironment::open(&session_bus)
        .await
        .context("Failed to query systemd for environment")?;

    let path = env_path_merge(&systemd_env, &unix_env);

    if matches!(mode, Strategy::Systemd) {
        let mut env = EnvBuf::from_diff(systemd_env);
        env.apply(path);
        return Ok(env);
    };

    let mut merged = EnvBuf::new();
    merged.apply(systemd_env);
    merged.apply(unix_env);
    merged.apply(path);
    Ok(merged)
}
