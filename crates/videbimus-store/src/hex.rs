//! Lower-case hex for keys and digests, on disk and in manifests.

use videbimus_core::AssetKey;

/// The full 64-character hex form of a key: its directory name in the
/// store. ([`AssetKey`]'s `Display` shows only the first 16 characters.)
#[must_use]
pub fn key_hex(key: &AssetKey) -> String {
    encode(&key.0)
}

/// Parses a key from its 64-character hex form.
#[must_use]
pub fn parse_key(s: &str) -> Option<AssetKey> {
    decode(s).map(AssetKey)
}

pub(crate) fn encode(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub(crate) fn decode(s: &str) -> Option<[u8; 32]> {
    let s = s.as_bytes();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in s.chunks_exact(2).enumerate() {
        let hi = nibble(pair[0])?;
        let lo = nibble(pair[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Serde for `[u8; 32]` as hex.
pub(crate) mod digest {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(d: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::encode(d))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        super::decode(&s).ok_or_else(|| D::Error::custom(format!("not a 64-digit hex digest: {s}")))
    }
}

/// Serde for `Option<[u8; 32]>` as hex or null.
pub(crate) mod opt_digest {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    #[allow(clippy::ref_option)]
    pub fn serialize<S: Serializer>(d: &Option<[u8; 32]>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_some(&super::encode(d)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 32]>, D::Error> {
        let s = Option::<String>::deserialize(d)?;
        s.map(|s| {
            super::decode(&s)
                .ok_or_else(|| D::Error::custom(format!("not a 64-digit hex digest: {s}")))
        })
        .transpose()
    }
}

/// Serde for [`AssetKey`] as hex.
pub(crate) mod key {
    use serde::{Deserializer, Serializer};
    use videbimus_core::AssetKey;

    pub fn serialize<S: Serializer>(k: &AssetKey, s: S) -> Result<S::Ok, S::Error> {
        super::digest::serialize(&k.0, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<AssetKey, D::Error> {
        super::digest::deserialize(d).map(AssetKey)
    }
}
