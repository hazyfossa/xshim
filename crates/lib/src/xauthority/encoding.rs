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

#[binrw]
#[brw(little)]
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
