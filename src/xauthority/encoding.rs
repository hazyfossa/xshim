pub use binrw::BinWrite;
use binrw::binrw;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub type Result<T> = std::result::Result<T, binrw::Error>;

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

pub type Hostname = Vec<u8>;

fn bound_len<B: TryFrom<usize>, T>(value: &T, field: &str) -> Result<B> {
    size_of_val(value)
        .try_into()
        .map_err(|_| binrw::Error::Custom {
            pos: 0,
            err: Box::new(format!("overflow at field {field}")),
        })
}

#[binrw]
#[brw(little)]
#[derive(ZeroizeOnDrop)]
pub struct Entry {
    family: Family,

    #[bw(calc = {bound_len(&address, "address")?})]
    address_len: u16,
    #[br(count = address_len)]
    pub address: Hostname,

    #[bw(calc = {bound_len(&display, "display")?})]
    display_len: u16,
    #[br(count = display_len)]
    pub display: Vec<u8>,

    #[bw(calc = {bound_len(&name, "name")?})]
    name_len: u16,
    #[br(count = name_len)]
    pub name: Vec<u8>,

    #[bw(calc = {bound_len(&data, "data")?})]
    data_len: u16,
    #[br(count = data_len)]
    pub data: Vec<u8>,
}

pub enum Target {
    // u8 (256 cookies / displays) is an arbitrary but reasonable limit
    Server { slot: u8 },
    Client { display_number: u8 },
}

impl From<Target> for u8 {
    fn from(value: Target) -> Self {
        match value {
            Target::Server { slot } => slot,
            Target::Client { display_number } => display_number,
        }
    }
}

pub enum Scope {
    Local(Hostname),
    Any,
}

impl From<Scope> for (Family, Hostname) {
    fn from(value: Scope) -> Self {
        match value {
            Scope::Local(hostname) => (Family::Local, hostname),
            // TODO: address in little-endian
            Scope::Any => (Family::Wild, [127, 0, 0, 2].to_vec()),
        }
    }
}

// Technically, this should be a trait "AuthMethod"
// Practically, cookie is the only method that is currently used
#[derive(ZeroizeOnDrop)]
pub struct Cookie([u8; Self::BYTES_LEN]);
impl Cookie {
    pub const BYTES_LEN: usize = 16; // 16 * 8 = 128 random bits
    const AUTH_NAME: &str = "MIT-MAGIC-COOKIE-1";

    pub fn new(random_bytes: [u8; Self::BYTES_LEN]) -> Self {
        Self(random_bytes)
    }

    pub fn raw_data(&self) -> (Vec<u8>, Vec<u8>) {
        (Self::AUTH_NAME.into(), self.0.into())
    }
}

impl Entry {
    pub fn new(cookie: &Cookie, scope: Scope, target: Target) -> Entry {
        let (family, address) = scope.into();
        let display = vec![target.into()];
        let (name, data) = cookie.raw_data();

        Entry {
            family,
            address,
            display,
            name,
            data,
        }
    }
}
