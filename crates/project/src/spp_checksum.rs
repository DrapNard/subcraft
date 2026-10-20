//! Dataset checksum used by the currently observed SPP HDF5 attributes.
//!
//! This is the standard MurmurHash3 x64 128-bit algorithm, initialized with
//! `0xF13A0239` in both hash lanes. It hashes the dataset's raw bytes and
//! returns the two final lanes in their stored order. The algorithm follows
//! Austin Appleby's public-domain SMHasher reference:
//! <https://github.com/aappleby/smhasher/blob/master/src/MurmurHash3.cpp>.
//!
//! The SPP-specific seed and byte coverage are based on independent format
//! interoperability research, not Adobe documentation. Keep that distinction
//! clear when using this helper for other SPP structures or data types.

const SEED: u64 = 0xF13A_0239;
const C1: u64 = 0x87C3_7B91_1142_53D5;
const C2: u64 = 0x4CF5_AD43_2745_937F;
const FMIX1: u64 = 0xFF51_AFD7_ED55_8CCD;
const FMIX2: u64 = 0xC4CE_B9FE_1A85_EC53;

/// Hash raw dataset bytes with the observed SPP `m3_x64_128` parameters.
///
/// The return value is `[h1, h2]`, matching the order of the two unsigned
/// 64-bit words stored in the HDF5 attribute.
pub fn spp_dataset_checksum(data: &[u8]) -> [u64; 2] {
    hash_with_seed(data, SEED)
}

fn hash_with_seed(data: &[u8], seed: u64) -> [u64; 2] {
    let mut h1 = seed;
    let mut h2 = seed;

    let mut blocks = data.chunks_exact(16);
    for block in &mut blocks {
        let k1 = u64::from_le_bytes(block[..8].try_into().expect("8-byte block half"));
        let k2 = u64::from_le_bytes(block[8..].try_into().expect("8-byte block half"));

        h1 ^= mix_k1(k1);
        h1 = h1.rotate_left(27);
        h1 = h1
            .wrapping_add(h2)
            .wrapping_mul(5)
            .wrapping_add(0x52DC_E729);

        h2 ^= mix_k2(k2);
        h2 = h2.rotate_left(31);
        h2 = h2
            .wrapping_add(h1)
            .wrapping_mul(5)
            .wrapping_add(0x3849_5AB5);
    }

    let tail = blocks.remainder();
    let mut k1 = 0u64;
    let mut k2 = 0u64;
    for (index, &byte) in tail.iter().enumerate() {
        if index < 8 {
            k1 |= u64::from(byte) << (index * 8);
        } else {
            k2 |= u64::from(byte) << ((index - 8) * 8);
        }
    }
    if !tail.is_empty() {
        h1 ^= mix_k1(k1);
        if tail.len() > 8 {
            h2 ^= mix_k2(k2);
        }
    }

    let length = data.len() as u64;
    h1 ^= length;
    h2 ^= length;
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    h1 = fmix64(h1);
    h2 = fmix64(h2);
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);

    [h1, h2]
}

#[inline]
fn mix_k1(value: u64) -> u64 {
    value.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2)
}

#[inline]
fn mix_k2(value: u64) -> u64 {
    value.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1)
}

#[inline]
fn fmix64(mut value: u64) -> u64 {
    value ^= value >> 33;
    value = value.wrapping_mul(FMIX1);
    value ^= value >> 33;
    value = value.wrapping_mul(FMIX2);
    value ^ (value >> 33)
}

#[cfg(test)]
mod tests {
    use super::spp_dataset_checksum;

    #[test]
    fn matches_standard_seed_zero_vectors() {
        // Public MurmurHash3 x64_128 vectors from SMHasher, checked with seed 0.
        assert_eq!(super::hash_with_seed(b"", 0), [0, 0]);
        assert_eq!(
            super::hash_with_seed(b"hello", 0),
            [0xCBD8_A7B3_41BD_9B02, 0x5B1E_906A_48AE_1D19]
        );
    }

    #[test]
    fn matches_seeded_reference_vectors_for_every_short_tail_length() {
        // Generated with scripts/research/spp_hash_probe.py using the same
        // published SMHasher algorithm and (seed, seed) initial state.
        const EXPECTED: [[u64; 2]; 32] = [
            [0xc5a2b37eeb678e4c, 0x9d43b16b05878c5c],
            [0xae69456687bd0650, 0x8eac584d83da3329],
            [0x1d3f286bc27a78e3, 0xcb7cdac69edfdf4e],
            [0x8b3d92e8b4b282ea, 0x31e7ce712ebb36e6],
            [0xdfc0524e2602cb97, 0x9c19242c77509586],
            [0x8806e8f3636c3ea9, 0x51989aa6850ea44f],
            [0x9eb03cc509941bf3, 0x1fddf745ae5a44ca],
            [0x978c598a60300ac2, 0x021b4742e88907ab],
            [0x0d1656af5527b716, 0xe9e37f3c5442632a],
            [0x8d128353fe1720b9, 0xe888e2bf0e77cd60],
            [0xd63829b535233643, 0x32a3eecdb339f3bb],
            [0x7b5426391b10a424, 0xe721c09c415399fb],
            [0x601c75f2ab50c32b, 0x4df5c5d994507dc3],
            [0xe236ea9e412c545d, 0xb89c5a400098af01],
            [0x0056416ffa6f519d, 0x2114ad3752b28f3d],
            [0x2bf8409579a87f0f, 0xb953189e6f50861f],
            [0xbea3a1fed8779ba7, 0x744b4001f535fc89],
            [0x2b9c335f814bbcbd, 0x89836aec2ba8f17c],
            [0xee451feda9adb66e, 0xacc91aba5b78f460],
            [0x1d476c3216defc9b, 0xf45ba861bd47d047],
            [0xb915f5f36be56952, 0x724b2f4f70257d5e],
            [0x85662b6a2175921c, 0x308e7fb8c9e752f8],
            [0x064c0a60f7748987, 0x3f8aa46e368bcf29],
            [0xef192759640c656e, 0xe275aaf723b851b9],
            [0xd993ad030a357651, 0x6e7efbaa46e2c8df],
            [0xd21fe60df781ddf5, 0x1234bf67cd7d0fff],
            [0xcb46f46d6f303b99, 0x0dd814fbb097ac97],
            [0xa44e8bbae8452d67, 0x5448e3d854588318],
            [0x63f481704e0d95eb, 0x790a11e8353c3b6b],
            [0x560122992b9a7e33, 0x91dd98151c2eb93c],
            [0x6ea736b5face7de2, 0xc0ba6e901ddf15b8],
            [0x9b89be2e1e27a4b1, 0x6b92502b5159511a],
        ];

        for (length, expected) in EXPECTED.into_iter().enumerate() {
            let data: Vec<u8> = (0..length)
                .map(|index| (index as u8).wrapping_mul(17).wrapping_add(3))
                .collect();
            assert_eq!(spp_dataset_checksum(&data), expected, "length {length}");
        }
    }
}
