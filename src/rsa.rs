//! Vérification des signatures RSA des extensions Chrome (`.crx`, version 3) :
//! PKCS#1 v1.5 avec SHA-256, la forme que produit le Chrome Web Store.
//!
//! Écrite à la main (multiplication de Montgomery sur des mots de 32 bits)
//! plutôt qu'avec une bibliothèque RSA complète : Feather ne fait que
//! vérifier, jamais signer ni déchiffrer, donc rien de secret n'est manipulé
//! et le temps de calcul n'a rien à cacher.

/// Clé publique RSA : module et exposant, en mots de 32 bits, poids faibles d'abord.
pub struct PublicKey {
    n: Vec<u32>,
    e: u32,
}

/// Début de `DigestInfo` (ASN.1) pour SHA-256, avant les 32 octets de l'empreinte.
const SHA256_INFO: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20,
];

/// Identifiant de l'algorithme `rsaEncryption` (1.2.840.113549.1.1.1).
const RSA_OID: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];

/// Un élément DER : étiquette, contenu, et ce qui suit.
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let len = if first < 0x80 {
        usize::from(first)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return None;
        }
        let bytes = rest.get(..count)?;
        rest = &rest[count..];
        bytes.iter().fold(0usize, |len, &b| len << 8 | usize::from(b))
    };
    Some((tag, rest.get(..len)?, &rest[len..]))
}

/// Grand nombre écrit en octets (poids forts d'abord) → `k` mots, s'il y tient.
fn limbs(bytes: &[u8], k: usize) -> Option<Vec<u32>> {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let bytes = &bytes[start..];
    if bytes.len() > 4 * k {
        return None;
    }
    let mut out = vec![0u32; k];
    for (i, &b) in bytes.iter().rev().enumerate() {
        out[i / 4] |= u32::from(b) << (8 * (i % 4));
    }
    Some(out)
}

/// Lit une clé publique `SubjectPublicKeyInfo` (DER), comme celles des `.crx`.
/// Refuse les clés de moins de 1024 bits ou de plus de 8192, et les exposants
/// pairs ou démesurés (les vraies clés utilisent 65537).
pub fn public_key(spki: &[u8]) -> Option<PublicKey> {
    let (0x30, spki, _) = der(spki)? else { return None };
    let (0x30, algorithm, rest) = der(spki)? else { return None };
    let (0x06, oid, _) = der(algorithm)? else { return None };
    let (0x03, bits, _) = der(rest)? else { return None };
    let (0, rsa) = bits.split_first()? else { return None };
    let (0x30, rsa, _) = der(rsa)? else { return None };
    let (0x02, n, rest) = der(rsa)? else { return None };
    let (0x02, e, _) = der(rest)? else { return None };
    if oid != RSA_OID {
        return None;
    }
    let n = &n[n.iter().position(|&b| b != 0)?..];
    if !(128..=1024).contains(&n.len()) || n[n.len() - 1] & 1 == 0 {
        return None;
    }
    let e = limbs(e, 1)?[0];
    if e < 3 || e & 1 == 0 {
        return None;
    }
    Some(PublicKey { n: limbs(n, n.len().div_ceil(4))?, e })
}

/// Vrai si `signature` signe l'empreinte SHA-256 `digest` avec `key`.
pub fn verify_sha256(key: &PublicKey, digest: &[u8; 32], signature: &[u8]) -> bool {
    let n = &key.n;
    let len = n.len() * 4 - n[n.len() - 1].leading_zeros() as usize / 8;
    if signature.len() != len {
        return false;
    }
    let Some(s) = limbs(signature, n.len()).filter(|s| less(s, n)) else { return false };
    let m = to_bytes(&pow_mod(&s, key.e, n), len);
    // 00 01 FF…FF 00 DigestInfo(SHA-256) empreinte, sur toute la longueur du module.
    let mut expected = vec![0xff; len];
    expected[0] = 0;
    expected[1] = 1;
    let info = len - SHA256_INFO.len() - 32;
    expected[info - 1] = 0;
    expected[info..info + SHA256_INFO.len()].copy_from_slice(&SHA256_INFO);
    expected[len - 32..].copy_from_slice(digest);
    m == expected
}

/// Mots → `len` octets, poids forts d'abord.
fn to_bytes(limbs: &[u32], len: usize) -> Vec<u8> {
    (0..len).rev().map(|i| limbs.get(i / 4).map_or(0, |w| (w >> (8 * (i % 4))) as u8)).collect()
}

/// a < b (même nombre de mots).
fn less(a: &[u32], b: &[u32]) -> bool {
    a.iter().rev().cmp(b.iter().rev()) == std::cmp::Ordering::Less
}

/// a -= b, sur les mots de `b` (la retenue descend dans le mot suivant de `a`).
fn sub(a: &mut [u32], b: &[u32]) {
    let mut borrow = 0u64;
    for (i, x) in a.iter_mut().enumerate() {
        let y = u64::from(b.get(i).copied().unwrap_or(0)) + borrow;
        borrow = u64::from(u64::from(*x) < y);
        *x = (u64::from(*x).wrapping_sub(y)) as u32;
    }
}

/// a·b·R⁻¹ mod n, avec R = 2^(32·k) (multiplication de Montgomery, a et b < n).
fn mont_mul(a: &[u32], b: &[u32], n: &[u32], n0: u32) -> Vec<u32> {
    let k = n.len();
    let mut t = vec![0u32; k + 2];
    for &ai in a {
        let mut carry = 0u64;
        for j in 0..k {
            let x = u64::from(t[j]) + u64::from(ai) * u64::from(b[j]) + carry;
            t[j] = x as u32;
            carry = x >> 32;
        }
        let x = u64::from(t[k]) + carry;
        t[k] = x as u32;
        t[k + 1] = (x >> 32) as u32;
        // m annule le mot bas de t + m·n, qui se divise alors par 2^32.
        let m = t[0].wrapping_mul(n0);
        let mut carry = (u64::from(t[0]) + u64::from(m) * u64::from(n[0])) >> 32;
        for j in 1..k {
            let x = u64::from(t[j]) + u64::from(m) * u64::from(n[j]) + carry;
            t[j - 1] = x as u32;
            carry = x >> 32;
        }
        let x = u64::from(t[k]) + carry;
        t[k - 1] = x as u32;
        t[k] = t[k + 1] + (x >> 32) as u32;
    }
    // t < 2n : une soustraction au plus.
    if t[k] != 0 || !less(&t[..k], n) {
        sub(&mut t[..=k], n);
    }
    t.truncate(k);
    t
}

/// s^e mod n (n impair, s < n).
fn pow_mod(s: &[u32], e: u32, n: &[u32]) -> Vec<u32> {
    let k = n.len();
    // -n⁻¹ mod 2^32, par la méthode de Newton (chaque tour double les bits justes).
    let mut inverse = 1u32;
    for _ in 0..5 {
        inverse = inverse.wrapping_mul(2u32.wrapping_sub(n[0].wrapping_mul(inverse)));
    }
    let n0 = inverse.wrapping_neg();
    // R² mod n, en doublant 1 autant de fois que R² a de bits.
    let mut r2 = vec![0u32; k + 1];
    r2[0] = 1;
    for _ in 0..64 * k {
        let mut carry = 0;
        for w in r2.iter_mut() {
            let next = *w >> 31;
            *w = *w << 1 | carry;
            carry = next;
        }
        if r2[k] != 0 || !less(&r2[..k], n) {
            sub(&mut r2, n);
        }
    }
    r2.truncate(k);
    let mut one = vec![0u32; k];
    one[0] = 1;
    let base = mont_mul(s, &r2, n, n0);
    let mut acc = mont_mul(&one, &r2, n, n0);
    for bit in (0..32 - e.leading_zeros()).rev() {
        acc = mont_mul(&acc, &acc, n, n0);
        if e >> bit & 1 == 1 {
            acc = mont_mul(&acc, &base, n, n0);
        }
    }
    mont_mul(&acc, &one, n, n0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// Clé RSA de 2048 bits faite pour ces tests (openssl genrsa), et la
    /// signature de « Feather vérifie les extensions. » (openssl dgst -sha256 -sign).
    const KEY: &str = concat!(
        "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA7bivQBAIuORh9WJBpoeyyVk9xj0vLpV/FQ9oieq/IGNBaVDLU2Tl",
        "Ajz4ic/UQrK9RiFss7wbjwYYkmoN7Z9nl+qDjiMdmvW9oACpUXEzclnUUvXVyXbCw0hLU17xL/N55O1R9Vg0dx3W4OMFqs3l",
        "fGGI/sbcCk8qfVcpmj56ubO7LRRw9HGQR4V24PeoQEp5Jkn1pgQCzjlBYwNR3UStfDmqCEa1IsmJVHaU1Yc1LwgjBVPbSibz",
        "sMGSUTiHvyNIkRx/PEV86fH5CfeDoNn39ycE60YRqZom12ZCHVp02EnyWOXNfwYwmX6A3Xzn95K0wiSJWBYZK5b2hG95bnbk",
        "lQIDAQAB",
    );
    const SIGNATURE: &str = concat!(
        "pWQB0iepJJTT/fzAtmhPH0V8klkHvDnC70Tax/uwed+mhcL7zeR/e1G1Ln3pr2R9imQXNccrh9+eiLUKVahcrA2XzURcReeq",
        "SuJNSx3ctnRn5bpdP3wEnNPMYqQdw30xBHQIjFbcRWorzSgGgkHiJWDAcO/JgKVeNvU/aZFP/sMjYaOkyZiZGUDqwTljekUC",
        "Gf5D+bnNyEQDofwOKZKUX1hgDGWlaVqEXMKpV1an2NIuQL3nyOs1QmX1ouNzBFZOYcNqqoTB7zgygKmXMDps9w+5lmoW4ult",
        "Zq7Ebr7f+B17DGBRC9mtSaBi4Q33a877k+TsiPOeZy7iPE5CyuYgPw==",
    );
    const MESSAGE: &[u8] = b"Feather v\xc3\xa9rifie les extensions.";

    fn decode(text: &str) -> Vec<u8> {
        crate::extensions::base64_decode(text).unwrap()
    }

    #[test]
    fn puissances_comme_u128() {
        // Modules de 64 bits : le résultat se vérifie avec les entiers de Rust.
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200 {
            let n = next() | 1 | 1 << 63;
            let s = next() % n;
            let e = (next() as u32) | 1;
            let expected = {
                let (mut acc, mut base, mut e) = (1u128, u128::from(s), e);
                while e > 0 {
                    if e & 1 == 1 {
                        acc = acc * base % u128::from(n);
                    }
                    base = base * base % u128::from(n);
                    e >>= 1;
                }
                acc as u64
            };
            let limbs = |v: u64| vec![v as u32, (v >> 32) as u32];
            assert_eq!(pow_mod(&limbs(s), e, &limbs(n)), limbs(expected), "{s}^{e} mod {n}");
        }
    }

    #[test]
    fn signature_openssl() {
        let key = public_key(&decode(KEY)).unwrap();
        let digest: [u8; 32] = Sha256::digest(MESSAGE).into();
        let signature = decode(SIGNATURE);
        assert!(verify_sha256(&key, &digest, &signature));
        // Autre message, signature abîmée, tronquée, ou plus grande que le module.
        let other: [u8; 32] = Sha256::digest(b"autre chose").into();
        assert!(!verify_sha256(&key, &other, &signature));
        let mut bad = signature.clone();
        bad[100] ^= 1;
        assert!(!verify_sha256(&key, &digest, &bad));
        assert!(!verify_sha256(&key, &digest, &signature[1..]));
        assert!(!verify_sha256(&key, &digest, &[0xff; 256]));
    }

    #[test]
    fn cles_refusees() {
        assert!(public_key(b"").is_none());
        assert!(public_key(b"\x30\x03\x02\x01\x01").is_none());
        let mut key = decode(KEY);
        // Autre algorithme que RSA.
        let at = key.windows(RSA_OID.len()).position(|w| w == RSA_OID).unwrap();
        key[at + 8] = 2;
        assert!(public_key(&key).is_none());
    }
}
