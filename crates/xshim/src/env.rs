use envy::{define_env, parse::EnvironmentParse};

use crate::VtNumber;

define_env!(pub Display(u8) = #custom "DISPLAY");

impl Display {
    pub fn from_number(n: u8) -> Self {
        Self(n)
    }

    pub fn number(&self) -> u8 {
        self.0
    }
}

// TODO
#[derive(Debug)]
pub struct DisplayParseError;

impl std::error::Error for DisplayParseError {}
impl std::fmt::Display for DisplayParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Display is an invalid value")
    }
}

impl EnvironmentParse<String> for Display {
    type Error = DisplayParseError;

    fn env_serialize(self) -> String {
        format!(":{}", self.0)
    }

    fn env_deserialize(value: String) -> Result<Self, Self::Error> {
        let value = value.strip_prefix(":").ok_or(DisplayParseError)?;
        // .whatever_context("display should start with :")?;
        let value = value.parse().map_err(|_| DisplayParseError)?;

        Ok(Self(value))
    }
}

define_env!(pub WindowPath(String) = "WINDOWPATH");

impl WindowPath {
    pub fn previous_plus_vt(env: &impl envy::Get, vt: &VtNumber) -> Self {
        let previous = env.get::<Self>();
        Self(match previous {
            Ok(path) => format!("{}:{}", *path, *vt),
            Err(_) => vt.to_string(),
        })
    }
}
