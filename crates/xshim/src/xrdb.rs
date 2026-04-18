use std::path::PathBuf;

use eyre::{OptionExt, Result};
use x11rb::{
    reexports::x11rb_protocol::parse_display::ParsedDisplay,
    rust_connection::{DefaultStream, RustConnection as Connection},
};

use crate::{Display, xauthority::Cookie};

pub fn load_resources(
    display: &Display,
    cookie: &Cookie,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<()> {
    let xorg = xorg_connection(display, cookie)?;

    Ok(())
}
