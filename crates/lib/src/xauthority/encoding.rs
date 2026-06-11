use std::os::unix::ffi::OsStringExt;

use binrw::binrw;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::utils::hostname::Hostname;

#[binrw]
#[brw(repr(u16))]
#[derive(Zeroize)]
pub enum Family {
    Local = 256,
    Wild = 65535,

    Netname = 254,
    Krb5Principal = 253,
    LocalHost = 252,
}

// The .Xauthority file is a binary file consisting of a sequence of entries
// in the following format:
//
//	 2 bytes		Family value (second byte is as in protocol HOST)
//	 2 bytes		address length (always MSB first)
//	 A bytes		host address (as in protocol HOST)
//	 2 bytes		display "number" length (always MSB first)
//	 S bytes		display "number" string
//	 2 bytes		name length (always MSB first)
//	 N bytes		authorization name string
//	 2 bytes		data length (always MSB first)
//	 D bytes		authorization data string
#[binrw]
#[brw(big)]
#[derive(ZeroizeOnDrop, bon::Builder)]
pub struct Entry {
    #[builder(setters(vis = ""))]
    family: Family,

    #[builder(skip)]
    #[bw(try_calc = address.len().try_into())]
    address_len: u16,
    #[builder(setters(vis = ""))]
    #[br(count = address_len)]
    pub address: Vec<u8>,

    #[builder(skip)]
    #[bw(try_calc = display.len().try_into())]
    display_len: u16,
    #[builder(setters(vis = ""))]
    #[br(count = display_len)]
    pub display: Vec<u8>,

    #[builder(skip)]
    #[bw(try_calc = name.len().try_into())]
    name_len: u16,
    #[builder(setters(vis = ""))]
    #[br(count = name_len)]
    pub name: Vec<u8>,

    #[builder(skip)]
    #[bw(try_calc = data.len().try_into())]
    data_len: u16,
    #[builder(setters(vis = ""))]
    #[br(count = data_len)]
    pub data: Vec<u8>,
}

use entry_builder as b;
impl<S: b::State> EntryBuilder<S> {
    pub fn cookie(self, cookie: Cookie) -> EntryBuilder<b::SetData<b::SetName<S>>>
    where
        S::Data: b::IsUnset,
        S::Name: b::IsUnset,
    {
        let name = Cookie::AUTH_NAME.to_string().into_bytes();
        let data = cookie.0.to_vec();
        self.name(name).data(data)
    }

    /// For server authority files, slot is an arbitrary identifier
    /// the only requirement is that slots do not repeat in the same file
    ///
    /// For client authority files, slot is display number
    pub fn target(self, slot: u16) -> EntryBuilder<b::SetDisplay<S>>
    where
        S::Display: b::IsUnset,
    {
        self.display(slot.to_string().into_bytes())
    }

    pub fn scope(self, scope: Scope) -> EntryBuilder<b::SetAddress<b::SetFamily<S>>>
    where
        S::Address: b::IsUnset,
        S::Family: b::IsUnset,
    {
        match scope {
            Scope::Local(hostname) => self.family(Family::Local).address(hostname.into_vec()),
            Scope::Any => self.family(Family::Wild).address("127.0.0.2".into()),
        }
    }
}

// Technically, this should be a trait "AuthMethod"
// Practically, cookie is the only method that is currently used
#[derive(ZeroizeOnDrop, Clone)]
pub struct Cookie(pub(crate) [u8; Self::BYTES_LEN]);
impl Cookie {
    pub const BYTES_LEN: usize = 16; // 16 * 8 = 128 random bits
    pub const AUTH_NAME: &str = "MIT-MAGIC-COOKIE-1";
}

pub enum Scope {
    Local(Hostname),
    Any,
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::{BinWrite, io::NoSeek, meta::WriteEndian};
    use pretty_assertions::assert_eq;

    fn write_to_vec<T>(value: T) -> Vec<u8>
    where
        T: BinWrite + WriteEndian,
        for<'a> T::Args<'a>: std::default::Default,
    {
        let bytes: Vec<u8> = Vec::with_capacity(size_of_val(&value));
        let mut writer = NoSeek::new(bytes);
        value.write(&mut writer).unwrap();
        writer.into_inner()
    }

    #[test]
    fn xauth_output_parity() {
        const XAUTH_OUT: &[u8] = &[
            0x01, 0x00, 0x00, 0x07, 0x64, 0x65, 0x73, 0x6b, 0x74, 0x6f, 0x70, 0x00, 0x01, 0x30,
            0x00, 0x12, 0x4d, 0x49, 0x54, 0x2d, 0x4d, 0x41, 0x47, 0x49, 0x43, 0x2d, 0x43, 0x4f,
            0x4f, 0x4b, 0x49, 0x45, 0x2d, 0x31, 0x00, 0x10, 0xb5, 0xb4, 0x90, 0x32, 0x32, 0x76,
            0x53, 0x7a, 0x14, 0x36, 0xd7, 0x7e, 0xa3, 0xec, 0xbd, 0x36,
        ];

        const COOKIE: Cookie = Cookie([
            0xb5, 0xb4, 0x90, 0x32, 0x32, 0x76, 0x53, 0x7a, 0x14, 0x36, 0xd7, 0x7e, 0xa3, 0xec,
            0xbd, 0x36,
        ]);

        const HOSTNAME: &str = "desktop";

        let entry = Entry::builder()
            .cookie(COOKIE)
            .target(0)
            .scope(Scope::Local(HOSTNAME.into()))
            .build();

        let bytes = write_to_vec(entry);

        assert_eq!(bytes.as_slice(), XAUTH_OUT)
    }
}
