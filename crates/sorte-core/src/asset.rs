//! References to images and sounds kept in the data dir's asset store, where each file is
//! named by the SHA-256 of its bytes (`assets/<sha256>.<ext>`).

use serde::{Deserialize, Serialize};

/// An image or sound in the asset store.
///
/// Both parts are checked when the reference is built or read from a file, so
/// [`file_name`](Self::file_name) is always a plain file name: a crafted list or import
/// cannot make the store read or write outside its folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "AssetRefFields")]
pub struct AssetRef {
    sha256: String,
    ext: String,
}

impl AssetRef {
    /// Longest extension accepted; real ones (`png`, `jpeg`, `webp`, `ogg`, `flac`) are short.
    const MAX_EXT_LEN: usize = 16;

    /// A reference from the hash's 64 hex digits and the file extension (ASCII letters and
    /// digits, without the dot). Both are stored lowercase.
    pub fn new(sha256: &str, ext: &str) -> Result<Self, AssetRefError> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AssetRefError::InvalidSha256(sha256.to_owned()));
        }
        if ext.is_empty() || ext.len() > Self::MAX_EXT_LEN || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(AssetRefError::InvalidExtension(ext.to_owned()));
        }
        Ok(Self { sha256: sha256.to_ascii_lowercase(), ext: ext.to_ascii_lowercase() })
    }

    /// The SHA-256 of the asset's bytes, as 64 lowercase hex digits.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The file extension, lowercase and without the dot.
    pub fn ext(&self) -> &str {
        &self.ext
    }

    /// The asset's file name in the store, `<sha256>.<ext>`.
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.sha256, self.ext)
    }
}

/// The serialized form, checked by [`AssetRef::new`] on the way in.
#[derive(Deserialize)]
struct AssetRefFields {
    sha256: String,
    ext: String,
}

impl TryFrom<AssetRefFields> for AssetRef {
    type Error = AssetRefError;

    fn try_from(fields: AssetRefFields) -> Result<Self, Self::Error> {
        Self::new(&fields.sha256, &fields.ext)
    }
}

/// Why an asset reference was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AssetRefError {
    /// The hash is not 64 hex digits.
    #[error("{0:?} is not a SHA-256 in hex")]
    InvalidSha256(String),
    /// The extension is empty, too long, or has characters other than ASCII letters and digits.
    #[error("{0:?} is not a file extension: use ASCII letters and digits, without the dot")]
    InvalidExtension(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn keeps_a_valid_reference_lowercase() {
        let asset = AssetRef::new(&HASH.to_ascii_uppercase(), "PNG").unwrap();
        assert_eq!(asset.sha256(), HASH);
        assert_eq!(asset.ext(), "png");
        assert_eq!(asset.file_name(), format!("{HASH}.png"));
    }

    #[test]
    fn refuses_bad_hashes() {
        let bad = [
            String::new(),
            HASH[1..].to_owned(),
            format!("{HASH}0"),
            HASH.replace('f', "g"),
            format!("../{}", &HASH[3..]),
        ];
        for hash in bad {
            assert_eq!(AssetRef::new(&hash, "png"), Err(AssetRefError::InvalidSha256(hash.clone())));
        }
    }

    #[test]
    fn refuses_extensions_that_are_not_plain() {
        for ext in ["", ".png", "png/", "../x", "p g", "pñg", "abcdefghijklmnopq"] {
            assert_eq!(AssetRef::new(HASH, ext), Err(AssetRefError::InvalidExtension(ext.to_owned())));
        }
    }

    #[test]
    fn serializes_as_its_two_fields() {
        let asset = AssetRef::new(HASH, "ogg").unwrap();
        let json = serde_json::json!({ "sha256": HASH, "ext": "ogg" });
        assert_eq!(serde_json::to_value(&asset).unwrap(), json);
        assert_eq!(serde_json::from_value::<AssetRef>(json).unwrap(), asset);
    }

    #[test]
    fn a_file_cannot_point_outside_the_store() {
        // Short enough to pass the length check: only the character check stops it.
        for ext in ["/../../a", "\\..\\..\\a"] {
            let json = serde_json::json!({ "sha256": HASH, "ext": ext });
            assert!(serde_json::from_value::<AssetRef>(json).is_err(), "{ext:?} was accepted");
        }
    }
}
