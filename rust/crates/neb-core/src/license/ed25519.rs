//! Ed25519 signature verification (RFC 8032).
//!
//! The workspace deliberately carries no crypto dependency, so verification is implemented here on
//! the standard 5×51-bit field representation. Only the operations a verifier needs exist:
//! decompression, group arithmetic, scalar multiplication and the SHA-512 reduction. Signing is
//! compiled for tests and for the `test-support` feature, never into a release binary.

use sha2::{Digest, Sha512};

const MASK51: u64 = (1 << 51) - 1;

/// A field element modulo `2^255 - 19`, five 51-bit limbs, little-endian.
type Fe = [u64; 5];

/// Scalar order of the Ed25519 group, little-endian.
const L: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
];

/// Compressed base point `B`.
const BASE_BYTES: [u8; 32] = [
    0x58, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
];

const ZERO: Fe = [0, 0, 0, 0, 0];
const ONE: Fe = [1, 0, 0, 0, 0];

/// `d = -121665 / 121666 mod p`.
const D: Fe = [
    929955233495203,
    466365720129213,
    1662059464998953,
    2033849074728123,
    1442794654840575,
];

/// `sqrt(-1) mod p`.
const SQRT_M1: Fe = [
    1718705420411056,
    234908883556509,
    2233514472574048,
    2117202627021982,
    765476049583133,
];

/// Carries the limbs of an unreduced field element back into `0..2^51`.
fn fe_carry(h: Fe) -> Fe {
    let mut h = h;

    // A result of a multiplication may leave limb 1 a hair above 2^51 after the top limb folds
    // back into limb 0. Three passes cost nothing here and leave every limb canonical, which the
    // `fe_tobytes` reduction relies on.
    for _ in 0..3 {
        let c0 = h[0] >> 51;
        h[0] &= MASK51;
        h[1] += c0;
        let c1 = h[1] >> 51;
        h[1] &= MASK51;
        h[2] += c1;
        let c2 = h[2] >> 51;
        h[2] &= MASK51;
        h[3] += c2;
        let c3 = h[3] >> 51;
        h[3] &= MASK51;
        h[4] += c3;
        let c4 = h[4] >> 51;
        h[4] &= MASK51;
        h[0] += c4 * 19;
    }

    h
}

fn fe_add(a: &Fe, b: &Fe) -> Fe {
    fe_carry([
        a[0] + b[0],
        a[1] + b[1],
        a[2] + b[2],
        a[3] + b[3],
        a[4] + b[4],
    ])
}

fn fe_sub(a: &Fe, b: &Fe) -> Fe {
    // 2^52 - 38 = 4 * (2^50 - 9.5); the standard reference uses 2^52 - 38 in limb 0 and
    // 2^52 - 2 elsewhere so every limb stays non-negative before the carry pass.
    fe_carry([
        a[0] + 0x000F_FFFF_FFFF_FFDA - b[0],
        a[1] + 0x000F_FFFF_FFFF_FFFE - b[1],
        a[2] + 0x000F_FFFF_FFFF_FFFE - b[2],
        a[3] + 0x000F_FFFF_FFFF_FFFE - b[3],
        a[4] + 0x000F_FFFF_FFFF_FFFE - b[4],
    ])
}

fn fe_neg(a: &Fe) -> Fe {
    fe_sub(&ZERO, a)
}

fn fe_mul(a: &Fe, b: &Fe) -> Fe {
    let a0 = u128::from(a[0]);
    let a1 = u128::from(a[1]);
    let a2 = u128::from(a[2]);
    let a3 = u128::from(a[3]);
    let a4 = u128::from(a[4]);

    let b0 = u128::from(b[0]);
    let b1 = u128::from(b[1]);
    let b2 = u128::from(b[2]);
    let b3 = u128::from(b[3]);
    let b4 = u128::from(b[4]);

    let b1_19 = b1 * 19;
    let b2_19 = b2 * 19;
    let b3_19 = b3 * 19;
    let b4_19 = b4 * 19;

    let mut t = [
        a0 * b0 + a1 * b4_19 + a2 * b3_19 + a3 * b2_19 + a4 * b1_19,
        a0 * b1 + a1 * b0 + a2 * b4_19 + a3 * b3_19 + a4 * b2_19,
        a0 * b2 + a1 * b1 + a2 * b0 + a3 * b4_19 + a4 * b3_19,
        a0 * b3 + a1 * b2 + a2 * b1 + a3 * b0 + a4 * b4_19,
        a0 * b4 + a1 * b3 + a2 * b2 + a3 * b1 + a4 * b0,
    ];

    let mask = u128::from(MASK51);
    for index in 0..5 {
        let carry = t[index] >> 51;
        t[index] &= mask;
        if index + 1 < 5 {
            t[index + 1] += carry;
        } else {
            t[0] += carry * 19;
        }
    }

    let carry = t[0] >> 51;
    t[0] &= mask;
    t[1] += carry;

    [
        t[0] as u64,
        t[1] as u64,
        t[2] as u64,
        t[3] as u64,
        t[4] as u64,
    ]
}

fn fe_sq(a: &Fe) -> Fe {
    fe_mul(a, a)
}

fn fe_frombytes(bytes: &[u8; 32]) -> Fe {
    let load = |offset: usize| {
        let mut chunk = [0u8; 8];
        chunk.copy_from_slice(&bytes[offset..offset + 8]);
        u64::from_le_bytes(chunk)
    };

    fe_carry([
        load(0) & MASK51,
        (load(6) >> 3) & MASK51,
        (load(12) >> 6) & MASK51,
        (load(19) >> 1) & MASK51,
        (load(24) >> 12) & MASK51,
    ])
}

fn fe_tobytes(fe: &Fe) -> [u8; 32] {
    let mut h = fe_carry(*fe);

    // Reduce the 2^255 - 19 excess so the encoding is canonical.
    let mut q = (h[0] + 19) >> 51;
    q = (h[1] + q) >> 51;
    q = (h[2] + q) >> 51;
    q = (h[3] + q) >> 51;
    q = (h[4] + q) >> 51;

    h[0] += 19 * q;
    // The addition above stands in for subtracting p: the carry out of bit 255 is exactly the
    // hidden 2^255 and is dropped, not folded back like a multiplication carry.
    for index in 0..4 {
        let carry = h[index] >> 51;
        h[index] &= MASK51;
        h[index + 1] += carry;
    }
    h[4] &= MASK51;

    let mut out = [0u8; 32];
    let mut accumulator: u128 = 0;
    let mut bits = 0u32;
    let mut index = 0usize;

    for limb in h {
        accumulator |= u128::from(limb) << bits;
        bits += 51;
        while bits >= 8 {
            if index < 32 {
                out[index] = accumulator as u8;
                index += 1;
            }
            accumulator >>= 8;
            bits -= 8;
        }
    }

    if index < 32 {
        out[index] = accumulator as u8;
    }

    out
}

fn fe_isnegative(fe: &Fe) -> u8 {
    fe_tobytes(fe)[0] & 1
}

fn fe_iszero(fe: &Fe) -> bool {
    fe_tobytes(fe) == [0u8; 32]
}

fn fe_equal(a: &Fe, b: &Fe) -> bool {
    fe_tobytes(a) == fe_tobytes(b)
}

/// `a^exponent` for a little-endian 256-bit exponent.
fn fe_pow(a: &Fe, exponent: &[u8; 32]) -> Fe {
    let mut result = ONE;
    for bit in (0..256).rev() {
        result = fe_sq(&result);
        if (exponent[bit / 8] >> (bit % 8)) & 1 == 1 {
            result = fe_mul(&result, a);
        }
    }
    result
}

fn fe_invert(a: &Fe) -> Fe {
    // p - 2
    let mut exponent = [0xffu8; 32];
    exponent[0] = 0xeb;
    exponent[31] = 0x7f;
    fe_pow(a, &exponent)
}

fn fe_pow22523(a: &Fe) -> Fe {
    // (p - 5) / 8 = 2^252 - 3
    let mut exponent = [0xffu8; 32];
    exponent[0] = 0xfd;
    exponent[31] = 0x0f;
    fe_pow(a, &exponent)
}

/// A point in extended twisted Edwards coordinates.
#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

fn point_identity() -> Point {
    Point {
        x: ZERO,
        y: ONE,
        z: ONE,
        t: ZERO,
    }
}

fn point_add(a: &Point, b: &Point) -> Point {
    let aa = fe_mul(&fe_sub(&a.y, &a.x), &fe_sub(&b.y, &b.x));
    let bb = fe_mul(&fe_add(&a.y, &a.x), &fe_add(&b.y, &b.x));
    let two_d = fe_add(&D, &D);
    let cc = fe_mul(&fe_mul(&two_d, &a.t), &b.t);
    let dd = fe_add(&fe_mul(&a.z, &b.z), &fe_mul(&a.z, &b.z));

    let e = fe_sub(&bb, &aa);
    let f = fe_sub(&dd, &cc);
    let g = fe_add(&dd, &cc);
    let h = fe_add(&bb, &aa);

    Point {
        x: fe_mul(&e, &f),
        y: fe_mul(&g, &h),
        t: fe_mul(&e, &h),
        z: fe_mul(&f, &g),
    }
}

fn point_double(a: &Point) -> Point {
    let aa = fe_sq(&a.x);
    let bb = fe_sq(&a.y);
    let z_squared = fe_sq(&a.z);
    let cc = fe_add(&z_squared, &z_squared);
    let dd = fe_neg(&aa);
    let summed = fe_add(&a.x, &a.y);
    let e = fe_sub(&fe_sub(&fe_sq(&summed), &aa), &bb);
    let g = fe_add(&dd, &bb);
    let f = fe_sub(&g, &cc);
    let h = fe_sub(&dd, &bb);

    Point {
        x: fe_mul(&e, &f),
        y: fe_mul(&g, &h),
        t: fe_mul(&e, &h),
        z: fe_mul(&f, &g),
    }
}

/// `[scalar] point`, with a plain little-endian 256-bit scalar.
fn point_mul(point: &Point, scalar: &[u8; 32]) -> Point {
    let mut result = point_identity();
    for bit in (0..256).rev() {
        result = point_double(&result);
        if (scalar[bit / 8] >> (bit % 8)) & 1 == 1 {
            result = point_add(&result, point);
        }
    }
    result
}

fn point_compress(point: &Point) -> [u8; 32] {
    let z_inverse = fe_invert(&point.z);
    let x = fe_mul(&point.x, &z_inverse);
    let y = fe_mul(&point.y, &z_inverse);
    let mut bytes = fe_tobytes(&y);
    bytes[31] |= fe_isnegative(&x) << 7;
    bytes
}

fn point_decompress(bytes: &[u8; 32]) -> Option<Point> {
    let sign = bytes[31] >> 7;
    let mut y_bytes = *bytes;
    y_bytes[31] &= 0x7f;

    let y = fe_frombytes(&y_bytes);
    let y_squared = fe_sq(&y);
    let u = fe_sub(&y_squared, &ONE);
    let v = fe_add(&fe_mul(&y_squared, &D), &ONE);
    let v3 = fe_mul(&fe_sq(&v), &v);
    let v7 = fe_mul(&fe_sq(&v3), &v);

    let mut x = fe_mul(&fe_mul(&u, &v3), &fe_pow22523(&fe_mul(&u, &v7)));

    if !fe_equal(&fe_mul(&v, &fe_sq(&x)), &u) {
        x = fe_mul(&x, &SQRT_M1);
        if !fe_equal(&fe_mul(&v, &fe_sq(&x)), &u) {
            return None;
        }
    }

    if fe_isnegative(&x) != sign {
        x = fe_neg(&x);
    }

    // A `1` sign bit with x = 0 is not a canonical encoding of a point.
    if sign == 1 && fe_iszero(&x) {
        return None;
    }

    Some(Point {
        x,
        y,
        z: ONE,
        t: fe_mul(&x, &y),
    })
}

/// Reduces a little-endian 512-bit integer modulo the group order.
fn reduce_wide(bytes: &[u8; 64]) -> [u8; 32] {
    let mut remainder = [0u8; 33];

    for bit in (0..512).rev() {
        // remainder <<= 1, bringing in the next bit of the dividend.
        let incoming = (bytes[bit / 8] >> (bit % 8)) & 1;
        let mut carry = incoming;
        for byte in &mut remainder {
            let next = *byte >> 7;
            *byte = (*byte << 1) | carry;
            carry = next;
        }
        if carry == 1 {
            // Cannot happen for a 512-bit dividend in a 33-byte remainder, but keep it total.
            remainder[32] |= 1;
        }

        if cmp_le(&remainder, &L) != std::cmp::Ordering::Less {
            sub_le(&mut remainder, &L);
        }
    }

    let mut out = [0u8; 32];
    out.copy_from_slice(&remainder[..32]);
    out
}

fn cmp_le(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    for index in (0..a.len().max(b.len())).rev() {
        let left = a.get(index).copied().unwrap_or(0);
        let right = b.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Subtracts `b` from `a` in place; `a` must be at least `b`.
fn sub_le(a: &mut [u8], b: &[u8]) {
    let mut borrow = 0i16;
    for (index, byte) in a.iter_mut().enumerate() {
        let left = i16::from(*byte);
        let right = i16::from(b.get(index).copied().unwrap_or(0));
        let mut value = left - right - borrow;
        if value < 0 {
            value += 256;
            borrow = 1;
        } else {
            borrow = 0;
        }
        *byte = value as u8;
    }
}

fn scalar_is_canonical(scalar: &[u8; 32]) -> bool {
    cmp_le(scalar, &L) == std::cmp::Ordering::Less
}

fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Verifies `signature` over `message` with a 32-byte public key.
///
/// Returns `false` for every malformed input: a non-canonical scalar, a point that does not
/// decompress, or a signature that simply does not match.
pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let mut s = [0u8; 32];
    s.copy_from_slice(&signature[32..]);
    if !scalar_is_canonical(&s) {
        return false;
    }

    let Some(public_point) = point_decompress(public_key) else {
        return false;
    };
    let mut r_bytes = [0u8; 32];
    r_bytes.copy_from_slice(&signature[..32]);
    let Some(r_point) = point_decompress(&r_bytes) else {
        return false;
    };
    let Some(base) = point_decompress(&BASE_BYTES) else {
        return false;
    };

    let k = reduce_wide(&sha512(&[&r_bytes, public_key, message]));

    let left = point_mul(&base, &s);
    let right = point_add(&r_point, &point_mul(&public_point, &k));

    point_compress(&left) == point_compress(&right)
}

/// Public key for a 32-byte seed. Test and stub-server support only.
#[cfg(any(test, feature = "test-support"))]
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    let hashed = sha512(&[seed]);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&hashed[..32]);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;

    let base = point_decompress(&BASE_BYTES).expect("the base point is valid");
    point_compress(&point_mul(&base, &scalar))
}

/// Signs `message` with a 32-byte seed. Test and stub-server support only.
#[cfg(any(test, feature = "test-support"))]
pub fn sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    let hashed = sha512(&[seed]);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&hashed[..32]);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;

    let public = public_key(seed);
    let nonce = reduce_wide(&sha512(&[&hashed[32..], message]));
    let base = point_decompress(&BASE_BYTES).expect("the base point is valid");
    let r_encoded = point_compress(&point_mul(&base, &nonce));

    let k = reduce_wide(&sha512(&[&r_encoded, &public, message]));
    let s = scalar_mul_add(&k, &scalar, &nonce);

    let mut signature = [0u8; 64];
    signature[..32].copy_from_slice(&r_encoded);
    signature[32..].copy_from_slice(&s);
    signature
}

/// Eight little-endian 32-bit limbs of a 32-byte scalar.
#[cfg(any(test, feature = "test-support"))]
fn limbs32(bytes: &[u8; 32]) -> [u64; 8] {
    let mut limbs = [0u64; 8];
    for (index, limb) in limbs.iter_mut().enumerate() {
        let mut chunk = [0u8; 4];
        chunk.copy_from_slice(&bytes[index * 4..index * 4 + 4]);
        *limb = u64::from(u32::from_le_bytes(chunk));
    }
    limbs
}

/// `(a * b + c) mod L`, with 32-byte little-endian inputs. Test support only.
#[cfg(any(test, feature = "test-support"))]
fn scalar_mul_add(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let left = limbs32(a);
    let right = limbs32(b);
    let mut product = [0u64; 16];

    const MASK32: u64 = 0xffff_ffff;

    for i in 0..8 {
        let mut carry = 0u64;
        for j in 0..8 {
            let value = (product[i + j] & MASK32) + left[i] * right[j] + carry;
            product[i + j] = value & MASK32;
            carry = value >> 32;
        }

        let mut position = i + 8;
        while carry != 0 && position < product.len() {
            let value = product[position] + carry;
            product[position] = value & MASK32;
            carry = value >> 32;
            position += 1;
        }
    }

    let mut wide = [0u8; 64];
    for (index, limb) in product.iter().enumerate() {
        wide[index * 4..index * 4 + 4].copy_from_slice(&(*limb as u32).to_le_bytes());
    }

    let mut carry = 0u16;
    for index in 0..64 {
        let addend = if index < 32 { u16::from(c[index]) } else { 0 };
        let value = u16::from(wide[index]) + addend + carry;
        wide[index] = value as u8;
        carry = value >> 8;
    }

    reduce_wide(&wide)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
            .collect()
    }

    fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
        bytes.try_into().expect("length")
    }

    /// RFC 8032, section 7.1, test 1.
    #[test]
    fn an_rfc_8032_signature_verifies() {
        let public: [u8; 32] = array(&hex(
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        ));
        let signature: [u8; 64] = array(&hex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ));

        assert!(verify(&public, b"", &signature));
        assert!(!verify(&public, b"x", &signature));
    }

    /// RFC 8032, section 7.1, test 2 (one-byte message).
    #[test]
    fn a_second_rfc_8032_vector_verifies() {
        let public: [u8; 32] = array(&hex(
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        ));
        let signature: [u8; 64] = array(&hex(
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ));

        assert!(verify(&public, b"\x72", &signature));
    }

    #[test]
    fn a_tampered_signature_is_refused() {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let mut signature = sign(&seed, b"payload");

        assert!(verify(&public, b"payload", &signature));

        signature[0] ^= 1;
        assert!(!verify(&public, b"payload", &signature));
    }

    #[test]
    fn a_malformed_public_key_is_refused_without_panicking() {
        let mut public = [0xffu8; 32];
        assert!(!verify(&public, b"", &[0u8; 64]));

        public[31] = 0x7f;
        let _ = verify(&public, b"", &[0u8; 64]);
    }

    #[test]
    fn the_base_point_round_trips() {
        let base = point_decompress(&BASE_BYTES).expect("base point");
        assert_eq!(point_compress(&base), BASE_BYTES);
    }

    #[test]
    fn the_scalar_reduction_reduces_the_group_order_to_zero() {
        let mut wide = [0u8; 64];
        wide[..32].copy_from_slice(&L);
        assert_eq!(reduce_wide(&wide), [0u8; 32]);

        wide[0] += 1;
        let mut one = [0u8; 32];
        one[0] = 1;
        assert_eq!(reduce_wide(&wide), one);
    }

    #[test]
    fn twice_the_base_point_is_the_known_encoding() {
        let mut two = [0u8; 32];
        two[0] = 2;
        let base = point_decompress(&BASE_BYTES).expect("base point");
        let doubled = point_compress(&point_mul(&base, &two));

        assert_eq!(
            doubled,
            array(&hex(
                "c9a3f86aae465f0e56513864510f3997561fa2c9e85ea21dc2292309f3cd6022"
            ))
        );
    }

    #[test]
    fn adding_the_base_point_to_itself_matches_doubling() {
        let base = point_decompress(&BASE_BYTES).expect("base point");
        let sum = point_compress(&point_add(&base, &base));
        let doubled = point_compress(&point_double(&base));

        assert_eq!(sum, doubled, "add and double must agree");
        assert_eq!(
            sum,
            array(&hex(
                "c9a3f86aae465f0e56513864510f3997561fa2c9e85ea21dc2292309f3cd6022"
            )),
            "the sum of the base point with itself is 2B"
        );
    }

    #[test]
    fn a_signed_message_verifies_with_this_implementation() {
        let seed = [3u8; 32];
        let public = public_key(&seed);
        let signature = sign(&seed, b"hello");

        assert!(verify(&public, b"hello", &signature));
    }

    #[test]
    fn the_field_inverse_multiplies_back_to_one() {
        let value = fe_frombytes(&[9u8; 32]);
        let product = fe_mul(&value, &fe_invert(&value));
        assert!(
            fe_equal(&product, &ONE),
            "limbs={product:?} bytes={:?}",
            fe_tobytes(&product)
        );
    }

    #[test]
    fn the_inverse_of_two_is_the_known_field_element() {
        let two = [2u64, 0, 0, 0, 0];
        let inverse = fe_invert(&two);
        let mut expected = [0xffu8; 32];
        expected[0] = 0xf7;
        expected[31] = 0x3f;

        assert_eq!(fe_tobytes(&inverse), expected);
    }

    #[test]
    fn the_field_arithmetic_matches_small_integers() {
        let two = [2u64, 0, 0, 0, 0];
        let three = [3u64, 0, 0, 0, 0];
        let four = [4u64, 0, 0, 0, 0];

        assert_eq!(fe_mul(&two, &two), four);
        assert_eq!(fe_add(&two, &three), [5, 0, 0, 0, 0]);
        assert!(fe_equal(&fe_sub(&four, &three), &ONE));
        assert_eq!(fe_mul(&ONE, &three), three);
        assert_eq!(fe_tobytes(&four)[0], 4);
    }
}
