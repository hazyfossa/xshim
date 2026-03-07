use std::{collections::HashMap, env as sys, ffi::OsString, path::PathBuf};

use anyhow::{Result, anyhow};

// TODO: zerocopy views

// Parse

pub trait EnvironmentParse<Repr>: Sized {
    fn env_serialize(self) -> Repr;
    fn env_deserialize(raw: Repr) -> Result<Self>;
}

impl<T: EnvironmentParse<String>> EnvironmentParse<OsString> for T {
    fn env_serialize(self) -> OsString {
        self.env_serialize().into()
    }

    fn env_deserialize(raw: OsString) -> Result<Self> {
        let value = raw
            .into_string()
            .map_err(|_| anyhow!("Variable contains invalid encoding"))?;

        Self::env_deserialize(value)
    }
}

macro_rules! env_parse_raw {
    ($t:ident => $ty:ty) => {
        impl EnvironmentParse<$ty> for $t {
            fn env_serialize(self) -> $ty {
                self.into()
            }

            fn env_deserialize(raw: $ty) -> Result<Self> {
                Ok(Self::from(raw))
            }
        }
    };
}

env_parse_raw!(PathBuf => OsString);
env_parse_raw!(OsString => OsString);
env_parse_raw!(String => String);

// Define

pub trait EnvironmentVariable: EnvironmentParse<OsString> {
    const KEY: &str;
}

pub use crate::_define_env as define_env;
#[macro_export]
macro_rules! _define_env {
    ($vis:vis $name:ident ($repr:ty) = auto parse $key:expr) => {
        impl EnvironmentParse<String> for $name
        {
            fn env_serialize(self) -> String {
                self.0.to_string()
            }

            fn env_deserialize(raw: String) -> Result<Self> {
                Ok(Self(raw.parse()?))
            }
        }

        $crate::_define_env!($vis $name ($repr) = $key);
    };

    ($vis:vis $name:ident ($repr:ty) = parse $key:expr) => {
        impl crate::frame::environment::EnvironmentParse<std::ffi::OsString> for $name {
            fn env_serialize(self) -> std::ffi::OsString { self.0.env_serialize() }

            fn env_deserialize(raw: std::ffi::OsString) -> anyhow::Result<Self> {
                Ok(Self(<$repr>::env_deserialize(raw)?))
            }
        }

        $crate::_define_env!($vis $name ($repr) = $key);
    };

    ($vis:vis $name:ident ($repr:ty) = $key:expr) => {
        $vis struct $name($repr);

        impl crate::frame::environment::EnvironmentVariable for $name {
            const KEY: &str = $key;
        }

        impl std::ops::Deref for $name {
            type Target = $repr;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    };
}

// Env containers

// Raw interface

type EnvEntry = (String, OsString);

pub trait EnvRaw {
    fn raw_get(&self, key: &str) -> Option<OsString>;
    fn raw_merge(&mut self, diff: impl EnvDiff);
}

// Typed

pub trait Env: EnvRaw + EnvDiff {
    fn get<T: EnvironmentVariable>(&self) -> Result<T> {
        let raw = self
            .raw_get(T::KEY)
            .ok_or(anyhow!("Variable {} does not exist", T::KEY))?;

        // TODO: zerocopy
        T::env_deserialize(raw.clone())
    }

    fn set<T: EnvDiff>(&mut self, e: T) {
        self.raw_merge(e);
    }
}

impl<T> Env for T where T: EnvRaw + EnvDiff {}

// Buf

pub struct EnvBuf(HashMap<String, OsString>);

impl EnvBuf {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn from_values(values: impl IntoIterator<Item = EnvEntry>) -> Self {
        Self(values.into_iter().collect())
    }
}

impl EnvRaw for EnvBuf {
    fn raw_get(&self, key: &str) -> Option<OsString> {
        // TODO-ref: zerocopy
        self.0.get(key).cloned()
    }

    fn raw_merge(&mut self, diff: impl EnvDiff) {
        self.0.extend(diff.to_env_diff());
    }
}

impl EnvDiff for EnvBuf {
    fn to_env_diff(self) -> impl IntoIterator<Item = (String, OsString)> {
        self.0
    }
}

// System

pub struct EnvOs {
    append_buf: EnvBuf,
}

impl EnvOs {
    /// This creates a new view os the system environment
    ///
    /// Keep in mind that setting a variable is scoped per view
    /// For example, in this case:
    /// ```
    /// let a = EnvOs::new_view();
    /// a.set("foo=bar")
    ///
    /// let b = EnvOs::new_view();
    /// let x = b.raw_get("foo")
    /// ```
    /// x will either be None, or what has been in the system's native env.
    ///
    /// This also means that changes to views won't affect the current process env,
    /// eliminating spooky action at a distance.
    ///
    /// If you want to concurrently share an env view across your system,
    /// you can do it much like with any other struct.
    /// A common approach for async is Arc<Mutex<...>>
    pub fn new_view() -> Self {
        Self {
            append_buf: EnvBuf::new(),
        }
    }
}

impl EnvRaw for EnvOs {
    fn raw_get(&self, key: &str) -> Option<OsString> {
        match self.append_buf.raw_get(key) {
            Some(set) => return Some(set),
            None => sys::var_os(key),
        }
    }

    fn raw_merge(&mut self, diff: impl EnvDiff) {
        self.append_buf.raw_merge(diff);
    }
}

impl EnvDiff for EnvOs {
    fn to_env_diff(self) -> impl IntoIterator<Item = (String, OsString)> {
        // TODO: verify that new overrides correctly
        // TODO: consider optimizing this for spawn (do not copy what kernel passes anyway)
        // NOTE: this ignores variables with non-utf8 keys
        sys::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value)))
            .chain(self.append_buf.to_env_diff())
    }
}

// Diff

pub trait EnvDiff {
    fn to_env_diff(self) -> impl IntoIterator<Item = (String, OsString)>;
}

impl<T: EnvironmentVariable> EnvDiff for T {
    fn to_env_diff(self) -> impl IntoIterator<Item = (String, OsString)> {
        [(Self::KEY.to_string(), self.env_serialize())]
    }
}

// NOTE: this is for untyped variables
// you would usually prefer typed ones instead
impl EnvDiff for &'static str {
    fn to_env_diff(self) -> impl IntoIterator<Item = (String, OsString)> {
        let parts: Vec<&str> = self.split("=").collect();
        if parts.len() != 2 {
            panic!("Invalid environment update: {self}");
        }

        [(parts[0].into(), parts[1].into())]
    }
}

#[rustfmt::skip]
mod env_container_variadics {
    use super::*;

    macro_rules! var_impl {
        ( $( $name:ident )+ ) => {
            #[allow(non_camel_case_types)]
            impl<$($name: EnvDiff),+> EnvDiff for ($($name,)+)
            {
                fn to_env_diff(self) -> impl IntoIterator<Item = (String, OsString)> {
                    let iter = std::iter::empty();
                    let ($($name,)+) = self;
                    $(let iter = iter.chain($name.to_env_diff());)+
                    iter
                }
            }
        };
    }

    var_impl!           { a b }
    var_impl!          { a b c }
    var_impl!         { a b c d }
    var_impl!        { a b c d e }
    var_impl!       { a b c d e f }
    var_impl!      { a b c d e f g }
    var_impl!     { a b c d e f g h }
    var_impl!    { a b c d e f g h i }
    var_impl!   { a b c d e f g h i j }
    var_impl!  { a b c d e f g h i j k }
    var_impl! { a b c d e f g h i j k l }
}

// VecExt

trait EnvVecExt: Env + Sized {
    fn to_vec(self) -> Vec<OsString> {
        self.to_env_diff()
            .into_iter()
            .map(|pair| {
                let mut merged = OsString::new();

                merged.push(pair.0);
                merged.push("=");
                merged.push(pair.1);

                merged
            })
            .collect()
    }
}

impl<T: Env> EnvVecExt for T {}
