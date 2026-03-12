use envy::{Env, define_env, parse::EnvironmentParse};
use snafu::{OptionExt, ResultExt};

define_env!(pub Seat(String) = "XDG_SEAT");
define_env!(pub VtNumber(u8) = "XDG_VTNR");

define_env!(pub Display(u8) = custom "DISPLAY");

impl Display {
    pub fn from_number(n: u8) -> Self {
        Self(n)
    }

    pub fn number(&self) -> u8 {
        self.0
    }
}

impl EnvironmentParse<String> for Display {
    type Error = snafu::Whatever;

    fn env_serialize(self) -> String {
        format!(":{}", self.0)
    }

    fn env_deserialize(value: String) -> Result<Self, Self::Error> {
        let value = value
            .strip_prefix(":")
            .whatever_context("display should start with :")?;

        Ok(Self(
            value
                .parse()
                .whatever_context("display should be an integer")?,
        ))
    }
}

define_env!(pub WindowPath(String) = "WINDOWPATH");

impl WindowPath {
    pub fn previous_plus_vt(env: &impl Env, vt: &VtNumber) -> Self {
        let previous = env.get::<Self>();
        Self(match previous {
            Ok(path) => format!("{}:{}", path.0, vt.0),
            Err(_) => vt.0.to_string(),
        })
    }
}
