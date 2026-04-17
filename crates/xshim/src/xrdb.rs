use std::path::PathBuf;

use eyre::{OptionExt, Result};
use x11rb::{
    reexports::x11rb_protocol::parse_display::ParsedDisplay,
    rust_connection::{DefaultStream, RustConnection as Connection},
};

use crate::{Display, xauthority::Cookie};

fn xorg_connection(display: &Display, cookie: &Cookie) -> Result<Connection> {
    let display = ParsedDisplay {
        host: "".into(), // Use hostname from XAuthorityManager?
        protocol: None,
        display: **display,
        screen: 0,
    };

    let conn = display.connect_instruction().find_map(|c| {
        let (stream, _) = DefaultStream::connect(&c).ok()?;
        Connection::connect_to_stream_with_auth_info(
            stream,
            0,
            Cookie::AUTH_NAME.into(),
            cookie.raw_data(),
        )
        .ok()
    });

    conn.ok_or_eyre("Failed to connect to Xorg")
}

pub fn load_resources(
    display: &Display,
    cookie: &Cookie,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<()> {
    let conn = xorg_connection(display, cookie)?;

    todo!();

    Ok(())
}
