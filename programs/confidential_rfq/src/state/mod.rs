pub mod market;
pub mod rfq;

use anchor_lang::prelude::*;
use std::num::NonZeroU32;

pub type MakerId = NonZeroU32;

pub const fn extend_key<const N: usize>(key: &[u8]) -> [u8; N] {
    let mut out = [0; N];
    let mut i = 0;
    while i < key.len() && i < N {
        out[i] = key[i];
        i += 1;
    }
    out
}
pub const fn extend_key_u64<const N: usize>(key: &[u8], value: u64) -> [u8; N] {
    let mut out = [0; N];
    let mut i = 0;
    while i < key.len() && i < N {
        out[i] = key[i];
        i += 1;
    }
    let value_bytes = value.to_le_bytes();
    while i < N && i < value_bytes.len() + key.len() {
        out[i] = value_bytes[i - key.len()];
        i += 1;
    }
    out
}

pub trait CurrentAccountVersion {
    const VERSION: u8;

    fn version(&self) -> u8;
}

#[cfg(test)]
mod test {
    #[test]
    fn test_extend() {
        const KEY: &[u8] = b"key";
        let extended = super::extend_key(KEY);
        assert_eq!(
            extended,
            [
                b'k', b'e', b'y', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0
            ]
        );

        let extended = super::extend_key_u64(KEY, 1 + (1 << 8));
        assert_eq!(
            extended,
            [
                b'k', b'e', b'y', 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0
            ]
        )
    }
}
