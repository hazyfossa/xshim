use crate::environment::EnvironmentParse;

mod environment;
mod runtime_dir;
mod xauthority;

environment::define_env!(pub VtNumber(u8) = "XDG_VTNR");

impl EnvironmentParse<String> for VtNumber {
    fn env_serialize(self) -> String {
        self.0.to_string()
    }

    fn env_deserialize(raw: String) -> anyhow::Result<Self> {
        Ok(Self(raw.parse()?))
    }
}

fn main() {
    println!("Hello, world!");
}
