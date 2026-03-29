use std::ffi::OsString;

use envy::{
    container::{EnvBuf, EnvContainer, MutableEnvContainer},
    diff::{Diff, Entry},
};
use eyre::{Context, OptionExt, Result};
use zbus::proxy;

use crate::utils::warn::WarnExt;

#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    fn unset_and_set_environment(&self, names: &[&str], assignments: &[&str]) -> zbus::Result<()>;

    #[zbus(property)]
    fn environment(&self) -> zbus::Result<Vec<String>>;
}

pub struct SystemdEnvironment {
    connection: ManagerProxy<'static>,
    buf: EnvBuf,
}

impl SystemdEnvironment {
    pub async fn open(session_bus: &zbus::Connection) -> Result<Self> {
        let connection = ManagerProxy::builder(session_bus)
            .cache_properties(proxy::CacheProperties::No)
            .build()
            .await
            .context("Failed to connect to systemd")?;

        let mut ret = Self {
            connection,
            buf: EnvBuf::new(),
        };

        ret.update().await?;

        Ok(ret)
    }

    pub async fn update(&mut self) -> Result<()> {
        let new = self
            .connection
            .environment()
            .await
            .context("Failed to read environment from systemd")?;

        let buf = EnvBuf::from_entries(new.iter().filter_map(|pair| {
            let (k, v) = pair
                .split_once("=")
                .ok_or_eyre(format!("Skipping {}: not a valid env entry", pair))
                .warn()?;

            Some(Entry::Set {
                key: k.into(),
                value: v.into(),
            })
        }));

        self.buf = buf;

        Ok(())
    }
}

impl EnvContainer for SystemdEnvironment {
    fn raw_get(&self, key: &str) -> Option<OsString> {
        self.buf.raw_get(key)
    }
}

impl Diff for SystemdEnvironment {
    fn to_env_diff(self) -> impl IntoIterator<Item = Entry> {
        self.buf.to_env_diff()
    }
}

fn entry_push(to: &mut Vec<String>, entry: Entry) {
    let key = entry.key().to_string();

    if let Some(entry) = entry
        .to_os_string()
        .into_string()
        .map_err(|_| {
            format!("Skipping passing variable {key} to systemd: could not convert to string",)
        })
        .warn()
    {
        to.push(entry);
    }
}

fn as_slices(vec: &Vec<String>) -> Vec<&str> {
    vec.iter().map(String::as_str).collect()
}

impl MutableEnvContainer for SystemdEnvironment {
    fn raw_merge(&mut self, diff: impl envy::diff::Diff) {
        let mut variables = Vec::new();
        let mut unsets = Vec::new();

        for entry in diff.to_env_diff() {
            let target = match entry {
                Entry::Set { .. } => &mut variables,
                Entry::Unset { .. } => &mut unsets,
            };

            entry_push(target, entry);
        }

        let connection = self.connection.clone();

        tokio::spawn(async move {
            let variables = as_slices(&variables);
            let unsets = as_slices(&unsets);

            connection
                .unset_and_set_environment(&variables, &unsets)
                .await
        });
    }
}
