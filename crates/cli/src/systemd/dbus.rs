use eyre::{Context, Result};
use zbus::proxy;

#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait DBusInterface {
    #[allow(clippy::type_complexity)]
    fn start_transient_unit(
        &self,
        name: &str,
        mode: &str,
        properties: &[&(&str, &zbus::zvariant::Value<'_>)],
        aux: &[&(&str, &[&(&str, &zbus::zvariant::Value<'_>)])],
    ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    fn unset_and_set_environment(&self, names: &[&str], assignments: &[&str]) -> zbus::Result<()>;

    #[zbus(property)]
    fn environment(&self) -> zbus::Result<Vec<String>>;
}

#[derive(Clone)]
#[allow(private_interfaces)]
pub struct Manager {
    pub dbus: DBusInterfaceProxy<'static>,
}

impl Manager {
    pub async fn connect_session() -> Result<Self> {
        let session_bus = zbus::Connection::session()
            .await
            .context("Failed to connect to DBus (session bus)")?;

        Self::open_on_connection(&session_bus).await
    }

    pub async fn open_on_connection(session_bus: &zbus::Connection) -> Result<Self> {
        let dbus = DBusInterfaceProxy::builder(session_bus)
            .cache_properties(proxy::CacheProperties::No)
            .build()
            .await
            .context("Failed to connect to systemd")?;

        Ok(Self { dbus })
    }
}

pub mod units {
    use zbus::zvariant::OwnedObjectPath;

    use super::*;

    pub enum UnitMode {
        Replace,
        Fail,
        Isolate,
        IgnoreDependencies,
        IgnoreRequirements,
    }

    impl Into<&'static str> for UnitMode {
        fn into(self) -> &'static str {
            match self {
                Self::Replace => "replace",
                Self::Fail => "fail",
                Self::Isolate => "isolate",
                Self::IgnoreDependencies => "ignore-dependencies",
                Self::IgnoreRequirements => "ignore-requirements",
            }
        }
    }

    impl Manager {
        pub async fn transient_unit(&self, name: &str, mode: UnitMode) -> Result<Unit> {
            // TODO: sensible properties builder

            let object = self
                .dbus
                .start_transient_unit(name, mode.into(), &[], &[])
                .await
                .context("Failed to start transient unit")?;

            Ok(Unit { object })
        }
    }

    pub struct Unit {
        object: OwnedObjectPath,
    }

    // TODO: unit interface
}

pub mod env {
    use super::*;
    use crate::utils::warn::WarnExt;

    use std::ffi::OsString;

    use envy::{
        container::{EnvBuf, EnvContainer, MutableEnvContainer},
        diff::{Diff, Entry},
    };
    use eyre::{Context, OptionExt};

    pub struct SystemdEnvironment {
        manager: Manager,
        buf: EnvBuf,
    }

    impl Manager {
        pub async fn env(&self) -> Result<env::SystemdEnvironment> {
            let mut ret = env::SystemdEnvironment {
                manager: self.clone(),
                buf: EnvBuf::new(),
            };

            ret.update().await?;

            Ok(ret)
        }
    }

    impl SystemdEnvironment {
        pub async fn update(&mut self) -> Result<()> {
            let new = self
                .manager
                .dbus
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
            self.buf.into_diff()
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

            let manager = self.manager.clone();

            tokio::spawn(async move {
                let variables = as_slices(&variables);
                let unsets = as_slices(&unsets);

                manager
                    .dbus
                    .unset_and_set_environment(&variables, &unsets)
                    .await
            });
        }
    }
}
