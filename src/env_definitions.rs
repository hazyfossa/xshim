use anyhow::{Result, anyhow};

use crate::frame::environment::{Env, EnvironmentParse, define_env};

define_env!(pub Seat(String) = parse "XDG_SEAT");
define_env!(pub VtNumber(u8) = auto parse "XDG_VTNR");

define_env!(pub Display(u8) = "DISPLAY");

impl Display {
    pub fn from_number(n: u8) -> Self {
        Self(n)
    }

    pub fn number(&self) -> u8 {
        self.0
    }
}

impl EnvironmentParse<String> for Display {
    fn env_serialize(self) -> String {
        format!(":{}", self.0).into()
    }

    fn env_deserialize(value: String) -> Result<Self> {
        Ok(Self(
            value
                .strip_prefix(":")
                .ok_or(anyhow!("display should start with :"))?
                .parse()?,
        ))
    }
}

define_env!(pub WindowPath(String) = parse "WINDOWPATH");

impl WindowPath {
    pub fn previous_plus_vt(env: &impl Env, vt: &VtNumber) -> Self {
        let previous = env.get::<Self>();
        Self(match previous {
            Ok(path) => format!("{}:{}", path.0, vt.0),
            Err(_) => vt.0.to_string(),
        })
    }
}
