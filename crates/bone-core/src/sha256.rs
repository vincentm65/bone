//! SHA-256, from the library the TLS stack already brings.
use ring::digest::{Context, SHA256};

pub(crate) fn digest(data: impl AsRef<[u8]>) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(data);
    h.finish()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// For hashing in several pieces.
pub(crate) struct Hasher(Context);

impl Hasher {
    pub fn new() -> Self {
        Self(Context::new(&SHA256))
    }

    pub fn update(&mut self, data: impl AsRef<[u8]>) {
        self.0.update(data.as_ref());
    }

    pub fn finish(self) -> [u8; 32] {
        self.0.finish().as_ref().try_into().expect("SHA-256 is 32 bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_answers_whole_and_in_pieces() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(hex(&digest("abc")), abc);
        assert_eq!(
            hex(&digest("")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let mut h = Hasher::new();
        h.update("a");
        h.update(b"bc");
        assert_eq!(hex(&h.finish()), abc);
    }
}
