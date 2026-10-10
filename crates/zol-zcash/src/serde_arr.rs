//! serde for fixed byte arrays longer than 32 (serialized as a byte sequence).

use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

pub fn serialize<S: Serializer, const N: usize>(a: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_bytes(a)
}

pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(d: D) -> Result<[u8; N], D::Error> {
    let v: Vec<u8> = Vec::deserialize(d)?;
    v.try_into()
        .map_err(|_| D::Error::custom("wrong array length"))
}
