//! CPace, ciphersuite CPACE-RISTRETTO255-SHA512.
//!
//! The trust root of the Client/Core pairing handshake: two parties sharing a
//! low-entropy secret (the 6-digit pairing code, used as the password PRS)
//! derive a strong shared key (ISK) that an offline dictionary attacker cannot
//! recover.
//!
//! Implemented per draft-irtf-cfrg-cpace-21 (April 2026), Section 8.3
//! (G_Ristretto255) plus Appendix B.3, in initiator/responder mode, so the
//! transcript is `transcript_ir` (Section 6.3 / 7.2). The draft's raw ISK is the
//! full 64-byte SHA-512 output; both endpoints truncate identically to 32 bytes
//! for the public key material, and the full value is reachable only from the
//! tests so the appendix vector reproduces byte for byte.
//!
//! # This is a byte-exact parity surface
//!
//! The Client runs the initiator and is not being ported, so this conforms to
//! `packages/loca-shared/src/crypto/pake.ts` rather than the other way round.
//! Conformance to the draft is necessary and not sufficient: the draft's one
//! appendix vector never reaches multi-byte LEB128, the generator-string zero-pad
//! clamp, or the confirmation tag, which is where two conforming-looking
//! implementations diverge. `testdata/pake-vectors.json` closes that gap with
//! outputs taken from the TypeScript itself. See the test module.
//!
//! # Why this is hand-written
//!
//! No published Rust crate implements this construction. `pake-cpace` and
//! `pake-cpace-embedded` port libsodium's CPace: DSI strings
//! `CPaceRistretto255-1` / `-2`, no `lv_cat` framing, and an ISK split from one
//! SHA-512 over `(K, Ya, Yb)`. `cpace` (hdevalence) derives its generator and key
//! through HKDF-SHA512 under a `cpace-r255` label. Both predate the draft's
//! framing entirely, so neither can be made to agree with the Client.
//! `curve25519-dalek` supplies the group and nothing above it.

use std::fmt;

use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::Identity;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha512};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use super::secretgen::fill_random;

/// `G_Ristretto255.DSI` (draft Section 8.3).
const DSI: &[u8] = b"CPaceRistretto255";
/// `G_Ristretto255.DSI_ISK` (draft Section 8.3).
const DSI_ISK: &[u8] = b"CPaceRistretto255_ISK";

/// SHA-512's input block size, which is what the generator-string zero padding
/// is sized to fill.
const HASH_BLOCK_BYTES: usize = 128;
/// Ristretto255's field size. `calculate_generator` hashes to twice this.
const FIELD_SIZE_BYTES: usize = 32;
/// `sample_scalar` draws this many bytes, then clears the bits above
/// `GROUP_SIZE_BITS`.
const GROUP_SIZE_BYTES: usize = 32;
const GROUP_SIZE_BITS: usize = 252;

type HmacSha256 = Hmac<Sha256>;

/// Everything that aborts a CPace run. The reasons are distinguished for tests
/// and for the log, never for the client: `pake_start` collapses all of them
/// into one `invalid_pairing_code`, because telling a caller *why* its message
/// was rejected is free information about the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CpaceError {
    /// The peer element is not a valid ristretto255 encoding, or not 32 bytes.
    Decode,
    /// The peer element is the identity. It decodes cleanly (all-zero bytes are
    /// the canonical identity encoding), so it has to be rejected by name.
    Identity,
    /// The shared point came out as the identity. Unreachable in a prime-order
    /// group with a non-identity input and a non-zero scalar, kept because the
    /// draft requires the check and a future refactor could reintroduce the case.
    IdentityResult,
    /// A confirmation field is longer than `lv2`'s 2-byte length prefix allows.
    FieldTooLong,
    /// The platform CSPRNG failed.
    Rng,
}

/// The derived key: the first 32 bytes of the draft's raw ISK.
///
/// Wrapped rather than passed as a bare array so it wipes on drop and cannot be
/// printed. The `Debug` is hand-written for exactly that reason: a derived
/// `Debug` would put the key into any `{:?}`, including one inside a panic
/// message added by a later refactor.
#[derive(Clone)]
pub(crate) struct Isk(Zeroizing<[u8; 32]>);

impl Isk {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Build one from fixed bytes, so a test can park a session without running
    /// a handshake first.
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Isk(Zeroizing::new(bytes))
    }
}

impl fmt::Debug for Isk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Isk(<REDACTED>)")
    }
}

/// Which side of the handshake a confirmation tag authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Client,
    Server,
}

impl Role {
    fn byte(self) -> u8 {
        match self {
            Role::Client => 0x43, // 'C'
            Role::Server => 0x53, // 'S'
        }
    }
}

// --- string encoding (draft Section 4 / 6) ----------------------------------

/// `prepend_len`: an LEB128 length prefix, then the data.
///
/// The multi-byte branch starts at 128 bytes. Production never reaches it (the
/// PRS is 6 digits and the channel identifier is a 44-character pin), which is
/// precisely why it is the branch worth a fixture.
fn push_prepend_len(data: &[u8], out: &mut Vec<u8>) {
    let mut len = data.len();
    loop {
        if len < 0x80 {
            out.push(len as u8);
            break;
        }
        out.push((len as u8 & 0x7f) | 0x80);
        len >>= 7;
    }
    out.extend_from_slice(data);
}

/// Encoded size of `prepend_len(data)` without building it, for the zero-pad
/// calculation.
fn prepend_len_size(len: usize) -> usize {
    let mut n = len;
    let mut prefix = 1;
    while n >= 0x80 {
        prefix += 1;
        n >>= 7;
    }
    prefix + len
}

/// `lv_cat`: the concatenation of each argument under `prepend_len`.
fn lv_cat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(parts.iter().map(|p| prepend_len_size(p.len())).sum());
    for part in parts {
        push_prepend_len(part, &mut out);
    }
    out
}

// --- generator (draft Section 8.1 / 8.3) ------------------------------------

/// `generator_string(DSI, PRS, CI, sid, s_in_bytes)`.
///
/// `len_zpad = MAX(0, s_in_bytes - len(prepend_len(PRS)) - len(prepend_len(DSI))
/// - 1)`. The padding fills the first hash block so the number of bytes hashed
/// does not depend on how short the password is. Note the zero-pad field is
/// itself length-prefixed, so a `len_zpad` of 0 still contributes one `0x00`.
pub(crate) fn generator_string(prs: &[u8], ci: &[u8], sid: &[u8]) -> Vec<u8> {
    let len_zpad = HASH_BLOCK_BYTES
        .saturating_sub(prepend_len_size(prs.len()))
        .saturating_sub(prepend_len_size(DSI.len()))
        .saturating_sub(1);
    let zeros = vec![0u8; len_zpad];
    lv_cat(&[DSI, prs, &zeros, ci, sid])
}

/// `calculate_generator`: hash the generator string to `2 * field_size` bytes
/// with SHA-512, then apply the ristretto255 element derivation.
///
/// The truncation to 64 bytes is a no-op on SHA-512 and is kept because the
/// draft states the width rather than the hash.
pub(crate) fn calculate_generator(prs: &[u8], ci: &[u8], sid: &[u8]) -> RistrettoPoint {
    let digest = Sha512::digest(generator_string(prs, ci, sid));
    let mut uniform = [0u8; 2 * FIELD_SIZE_BYTES];
    uniform.copy_from_slice(&digest[..2 * FIELD_SIZE_BYTES]);
    // RFC 9496 Section 4.3.4 element derivation, the same construction
    // `@noble/curves` reaches through `ristretto255_hasher.deriveToCurve`: split
    // at 32, mask bit 255 and reduce each half, Elligator each, add.
    RistrettoPoint::from_uniform_bytes(&uniform)
}

// --- scalars (draft Section 8.3) --------------------------------------------

/// Clear the bits above `group_size_bits`, then decode little-endian.
///
/// The clamped value is below `2^252`, which is below the group order, so the
/// reduction in `from_bytes_mod_order` is the identity map and the scalar is
/// canonical.
pub(crate) fn clamp_scalar_le(mut bytes: [u8; GROUP_SIZE_BYTES]) -> Scalar {
    let top_bits_to_clear = GROUP_SIZE_BYTES * 8 - GROUP_SIZE_BITS;
    bytes[GROUP_SIZE_BYTES - 1] &= 0xff_u8 >> top_bits_to_clear;
    Scalar::from_bytes_mod_order(bytes)
}

/// `sample_scalar`, recommended form: `group_size_bytes` random bytes, clamped.
///
/// Resamples a zero scalar. The odds are `2^-252`, and the reason is parity
/// rather than security: `@noble/curves` rejects a zero scalar by throwing while
/// dalek multiplies happily to the identity, so without this the two sides
/// disagree about whether that draw is an error.
pub(crate) fn sample_scalar() -> Result<Scalar, CpaceError> {
    loop {
        let mut bytes = [0u8; GROUP_SIZE_BYTES];
        fill_random(&mut bytes).map_err(|_| CpaceError::Rng)?;
        let scalar = clamp_scalar_le(bytes);
        if scalar != Scalar::ZERO {
            return Ok(scalar);
        }
    }
}

/// `scalar_mult_vfy(y, X)`: decode `X`, reject the identity, return
/// `encode(y * X)`.
///
/// The draft returns the identity on failure and requires the caller to abort;
/// this returns an error instead so a caller cannot forget the check.
pub(crate) fn scalar_mult_vfy(y: &Scalar, encoded_x: &[u8]) -> Result<[u8; 32], CpaceError> {
    let compressed = CompressedRistretto::from_slice(encoded_x).map_err(|_| CpaceError::Decode)?;
    let x = compressed.decompress().ok_or(CpaceError::Decode)?;
    // All-zero bytes are the canonical encoding of the identity, so decoding
    // succeeds and this is the only thing standing between a peer sending 32
    // zeros and a shared "secret" everybody can compute.
    if bool::from(x.ct_eq(&RistrettoPoint::identity())) {
        return Err(CpaceError::Identity);
    }
    let k = y * x;
    if bool::from(k.ct_eq(&RistrettoPoint::identity())) {
        return Err(CpaceError::IdentityResult);
    }
    Ok(k.compress().to_bytes())
}

// --- transcript and ISK (draft Section 6.3 / 7.2) ---------------------------

/// `transcript_ir(Ya, ADa, Yb, ADb) = lv_cat(Ya, ADa) || lv_cat(Yb, ADb)`.
pub(crate) fn transcript_ir(ya: &[u8], ada: &[u8], yb: &[u8], adb: &[u8]) -> Vec<u8> {
    let mut out = lv_cat(&[ya, ada]);
    out.extend_from_slice(&lv_cat(&[yb, adb]));
    out
}

/// `ISK = SHA-512(lv_cat(DSI_ISK, sid, K) || transcript_ir(...))`, the draft's
/// full 64-byte raw ISK.
pub(crate) fn compute_isk(
    sid: &[u8],
    k: &[u8],
    ya: &[u8],
    ada: &[u8],
    yb: &[u8],
    adb: &[u8],
) -> Zeroizing<[u8; 64]> {
    // The input carries the shared point, so it is wiped rather than left in a
    // freed allocation.
    let mut input = Zeroizing::new(lv_cat(&[DSI_ISK, sid, k]));
    input.extend_from_slice(&transcript_ir(ya, ada, yb, adb));
    let digest = Sha512::digest(input.as_slice());
    let mut raw = [0u8; 64];
    raw.copy_from_slice(&digest);
    Zeroizing::new(raw)
}

/// The first 32 bytes of the raw ISK: the public 256-bit key material both
/// endpoints agree on.
fn derive_key(raw_isk: &[u8; 64]) -> Isk {
    let mut key = [0u8; 32];
    key.copy_from_slice(&raw_isk[..32]);
    Isk(Zeroizing::new(key))
}

// --- the responder ----------------------------------------------------------

pub(crate) struct ResponderResult {
    /// Message B (Yb), the responder's public element.
    pub(crate) msg_b: [u8; 32],
    pub(crate) isk: Isk,
}

impl fmt::Debug for ResponderResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Carries the derived key, so it redacts wholesale rather than relying
        // on the field's own Debug staying redacted.
        f.write_str("ResponderResult(<REDACTED>)")
    }
}

/// Run CPace as the responder (party B): consume msgA (Ya) and the shared
/// password, producing msgB and the derived key.
///
/// `ci` is the channel identifier, a public value both parties must agree on:
/// it is folded into the generator, so the derived keys differ if the two sides
/// disagree. Core passes its own TLS certificate pin, which binds the key to the
/// channel. Associated data is empty in Loca and is not part of this signature.
pub(crate) fn responder(
    password: &str,
    sid: &[u8],
    msg_a: &[u8],
    ci: &[u8],
) -> Result<ResponderResult, CpaceError> {
    responder_with(password, sid, msg_a, ci, &[], &[], sample_scalar()?)
}

pub(crate) fn responder_with(
    password: &str,
    sid: &[u8],
    msg_a: &[u8],
    ci: &[u8],
    ada: &[u8],
    adb: &[u8],
    yb: Scalar,
) -> Result<ResponderResult, CpaceError> {
    let g = calculate_generator(password.as_bytes(), ci, sid);
    let msg_b = (yb * g).compress().to_bytes();
    // Verify the peer element and derive K before anything is returned.
    let k = scalar_mult_vfy(&yb, msg_a)?;
    let raw_isk = compute_isk(sid, &k, msg_a, ada, &msg_b, adb);
    Ok(ResponderResult {
        msg_b,
        isk: derive_key(&raw_isk),
    })
}

// --- channel-bound key confirmation -----------------------------------------

/// A 2-byte little-endian length prefix, keeping the confirmation fields
/// unambiguous.
fn push_lv2(data: &[u8], out: &mut Vec<u8>) -> Result<(), CpaceError> {
    let len = u16::try_from(data.len()).map_err(|_| CpaceError::FieldTooLong)?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(data);
    Ok(())
}

/// `HMAC-SHA256(isk, role || lv2(msgA) || lv2(msgB) || lv2(utf8(pin)))`.
///
/// `pin` is the value each side observed for the channel. Folding it in is what
/// detects a man in the middle that re-terminates TLS with its own certificate
/// while faithfully relaying the PAKE messages: the relayed handshake succeeds
/// and the confirmation does not.
pub(crate) fn confirm_tag(
    isk: &Isk,
    role: Role,
    msg_a: &[u8],
    msg_b: &[u8],
    pin: &str,
) -> Result<[u8; 32], CpaceError> {
    let mut mac = new_mac(isk);
    mac.update(&build_confirm_message(role, msg_a, msg_b, pin)?);
    let mut tag = [0u8; 32];
    tag.copy_from_slice(&mac.finalize().into_bytes());
    Ok(tag)
}

/// Constant-time verification of a confirmation tag.
///
/// `verify_slice` rejects a wrong length and a wrong value alike, matching the
/// TypeScript's length check followed by an XOR-accumulate compare.
pub(crate) fn verify_confirm(
    tag: &[u8],
    isk: &Isk,
    role: Role,
    msg_a: &[u8],
    msg_b: &[u8],
    pin: &str,
) -> Result<bool, CpaceError> {
    let mut mac = new_mac(isk);
    mac.update(&build_confirm_message(role, msg_a, msg_b, pin)?);
    Ok(mac.verify_slice(tag).is_ok())
}

fn build_confirm_message(
    role: Role,
    msg_a: &[u8],
    msg_b: &[u8],
    pin: &str,
) -> Result<Zeroizing<Vec<u8>>, CpaceError> {
    let mut msg = Zeroizing::new(Vec::with_capacity(
        7 + msg_a.len() + msg_b.len() + pin.len(),
    ));
    msg.push(role.byte());
    push_lv2(msg_a, &mut msg)?;
    push_lv2(msg_b, &mut msg)?;
    push_lv2(pin.as_bytes(), &mut msg)?;
    Ok(msg)
}

/// HMAC accepts a key of any length, so this cannot fail for a 32-byte ISK.
/// Written without `expect` so the clippy lint stays meaningful.
fn new_mac(isk: &Isk) -> HmacSha256 {
    match HmacSha256::new_from_slice(isk.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => unreachable!("HMAC-SHA256 accepts a key of any length"),
    }
}

// --- the initiator, which production never runs -----------------------------
//
// The Client is the initiator and is not being ported, so Core has no reason to
// carry this outside tests. It exists so the appendix vector can be reproduced
// end to end and so a round trip is testable at all, which is exactly why
// `pake.ts` keeps its `__test` seam. Compiled out of every shipping build.

#[cfg(test)]
pub(crate) struct Initiator {
    pub(crate) msg_a: [u8; 32],
    ya: Scalar,
    sid: Vec<u8>,
    ada: Vec<u8>,
    adb: Vec<u8>,
    used: bool,
}

#[cfg(test)]
pub(crate) fn initiator_with(
    password: &str,
    sid: &[u8],
    ci: &[u8],
    ada: &[u8],
    adb: &[u8],
    ya: Scalar,
) -> Initiator {
    let g = calculate_generator(password.as_bytes(), ci, sid);
    Initiator {
        msg_a: (ya * g).compress().to_bytes(),
        ya,
        sid: sid.to_vec(),
        ada: ada.to_vec(),
        adb: adb.to_vec(),
        used: false,
    }
}

#[cfg(test)]
impl Initiator {
    pub(crate) fn finish(&mut self, msg_b: &[u8]) -> Result<Isk, CpaceError> {
        assert!(!self.used, "CPace initiator already finished");
        self.used = true;
        let k = scalar_mult_vfy(&self.ya, msg_b)?;
        let raw = compute_isk(&self.sid, &k, &self.msg_a, &self.ada, msg_b, &self.adb);
        Ok(derive_key(&raw))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn hex(s: &str) -> Vec<u8> {
        ::hex::decode(s).unwrap()
    }

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&hex(s));
        out
    }

    fn random_scalar() -> Scalar {
        sample_scalar().unwrap()
    }

    // --- the CFRG appendix vector -------------------------------------------
    //
    // draft-irtf-cfrg-cpace-21, Appendix B.3 (ristretto255 / SHA-512),
    // initiator/responder mode (B.3.5). Every intermediate is asserted
    // separately, so a failure names the step that broke rather than only the
    // final key.

    struct Vec3;
    impl Vec3 {
        const PRS: &'static [u8] = b"Password";
        // CI = lv("A_initiator") || lv("B_responder")
        const CI: &'static str = "0b415f696e69746961746f720b425f726573706f6e646572";
        const SID: &'static str = "7e4b4791d6a8ef019b936c79fb7f2c57";
        const GEN_HASH: &'static str =
            "da6d3ddc8802fca9058755ffd3ebde08a9c2c74945901a258482a288b6663af0\
             6bf645c93cd1c51512307199c80e84908916d983b34af77205f90851a657ee27";
        const G: &'static str = "222b6b195fe84b1652badb6f6a3ae3d24341e7306967f0b8115b40d5698c7e56";
        const ADA: &'static [u8] = b"ADa";
        const YA: &'static str = "da3d23700a9e5699258aef94dc060dfda5ebb61f02a5ea77fad53f4ff0976d08";
        const YA_PUB: &'static str =
            "d6bac480f2c386c394efc7c47adb9925dcd2630b64f240c50f8d0eec482b9157";
        const ADB: &'static [u8] = b"ADb";
        const YB: &'static str = "d2316b454718c35362d83d69df6320f38578ed5984651435e2949762d900b80d";
        const YB_PUB: &'static str =
            "3ea7e0b19560d7c0b0f5734f63b955286dfa8232b5ebe63324e2d9e7433f7258";
        const K: &'static str = "80b69a8a76457ab6a4d7f887a4bf6b55a2f80ac19c333f917a05fc9887c8b40f";
        const TRANSCRIPT: &'static str =
            "20d6bac480f2c386c394efc7c47adb9925dcd2630b64f240c50f8d0eec482b9157\
             03414461203ea7e0b19560d7c0b0f5734f63b955286dfa8232b5ebe63324e2d9e7\
             433f725803414462";
        const ISK: &'static str = "b69effbf61b51d56401c0f65601abe428de8206feaaf0e32198896dc\
             ae7b35cd2b38950a39dfd5d4a79164614c2984f7daa460b588c1e80c\
             3fa2068af7900447";
    }

    const B3_GENERATOR_STRING: &str = "11435061636552697374726574746f3235350850617373776f726464";

    #[test]
    fn b3_vector_generator_string() {
        let sid = hex(Vec3::SID);
        let ci = hex(Vec3::CI);
        let got = generator_string(Vec3::PRS, &ci, &sid);
        let want = format!(
            "{B3_GENERATOR_STRING}{}{}",
            "00".repeat(100),
            "180b415f696e69746961746f720b425f726573706f6e646572107e4b4791d6a8ef019b936c79fb7f2c57"
        );
        assert_eq!(::hex::encode(&got), want);
    }

    #[test]
    fn b3_vector_generator_hash_and_point() {
        let sid = hex(Vec3::SID);
        let ci = hex(Vec3::CI);
        let digest = Sha512::digest(generator_string(Vec3::PRS, &ci, &sid));
        assert_eq!(
            ::hex::encode(digest),
            Vec3::GEN_HASH.replace(['\n', ' '], "")
        );
        let g = calculate_generator(Vec3::PRS, &ci, &sid);
        assert_eq!(::hex::encode(g.compress().to_bytes()), Vec3::G);
    }

    #[test]
    fn b3_vector_public_elements() {
        let sid = hex(Vec3::SID);
        let ci = hex(Vec3::CI);
        let g = calculate_generator(Vec3::PRS, &ci, &sid);
        let ya = clamp_scalar_le(hex32(Vec3::YA));
        let yb = clamp_scalar_le(hex32(Vec3::YB));
        assert_eq!(::hex::encode((ya * g).compress().to_bytes()), Vec3::YA_PUB);
        assert_eq!(::hex::encode((yb * g).compress().to_bytes()), Vec3::YB_PUB);
    }

    #[test]
    fn b3_vector_shared_point_from_both_directions() {
        let ya = clamp_scalar_le(hex32(Vec3::YA));
        let yb = clamp_scalar_le(hex32(Vec3::YB));
        let ka = scalar_mult_vfy(&ya, &hex(Vec3::YB_PUB)).unwrap();
        let kb = scalar_mult_vfy(&yb, &hex(Vec3::YA_PUB)).unwrap();
        assert_eq!(::hex::encode(ka), Vec3::K);
        assert_eq!(::hex::encode(kb), Vec3::K);
    }

    #[test]
    fn b3_vector_transcript_and_raw_isk() {
        let transcript =
            transcript_ir(&hex(Vec3::YA_PUB), Vec3::ADA, &hex(Vec3::YB_PUB), Vec3::ADB);
        assert_eq!(
            ::hex::encode(transcript),
            Vec3::TRANSCRIPT.replace(['\n', ' '], "")
        );
        let raw = compute_isk(
            &hex(Vec3::SID),
            &hex(Vec3::K),
            &hex(Vec3::YA_PUB),
            Vec3::ADA,
            &hex(Vec3::YB_PUB),
            Vec3::ADB,
        );
        assert_eq!(::hex::encode(*raw), Vec3::ISK.replace(['\n', ' '], ""));
    }

    #[test]
    fn b3_vector_end_to_end_through_both_roles() {
        let sid = hex(Vec3::SID);
        let ci = hex(Vec3::CI);
        let mut initiator = initiator_with(
            "Password",
            &sid,
            &ci,
            Vec3::ADA,
            Vec3::ADB,
            clamp_scalar_le(hex32(Vec3::YA)),
        );
        assert_eq!(::hex::encode(initiator.msg_a), Vec3::YA_PUB);

        let responder = responder_with(
            "Password",
            &sid,
            &initiator.msg_a,
            &ci,
            Vec3::ADA,
            Vec3::ADB,
            clamp_scalar_le(hex32(Vec3::YB)),
        )
        .unwrap();
        assert_eq!(::hex::encode(responder.msg_b), Vec3::YB_PUB);

        // The public key material is the raw ISK truncated to 32 bytes, and both
        // roles land on it.
        let want = &Vec3::ISK.replace(['\n', ' '], "")[..64];
        assert_eq!(::hex::encode(responder.isk.as_bytes()), want);
        let initiator_key = initiator.finish(&responder.msg_b).unwrap();
        assert_eq!(::hex::encode(initiator_key.as_bytes()), want);
    }

    // --- the differential fixtures ------------------------------------------

    #[derive(Deserialize)]
    struct Fixture {
        vectors: Vec<Vector>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Vector {
        note: String,
        password: String,
        sid: String,
        ci: String,
        ada: String,
        adb: String,
        ya: String,
        yb: String,
        pin: String,
        generator_string: String,
        g: String,
        msg_a: String,
        msg_b: String,
        k: String,
        transcript_ir: String,
        raw_isk: String,
        isk: String,
        confirm_c: String,
        confirm_s: String,
    }

    /// Replays every output the TypeScript produced.
    ///
    /// This is the test that matters. The appendix vector proves conformance to
    /// the draft; this proves conformance to the implementation the Client
    /// actually runs, across the cases the draft never reaches: the LEB128
    /// boundary at 127/128, the `len_zpad` clamp, `lv2`'s 2-byte prefix, the
    /// empty channel identifier used behind a proxy, and multi-byte UTF-8 in the
    /// password and the pin.
    #[test]
    fn typescript_fixtures_replay_exactly() {
        let fixture: Fixture =
            serde_json::from_str(include_str!("testdata/pake-vectors.json")).unwrap();
        assert!(
            fixture.vectors.len() >= 80,
            "fixture looks truncated: {} vectors",
            fixture.vectors.len()
        );

        for v in &fixture.vectors {
            let ctx = &v.note;
            let sid = hex(&v.sid);
            let ci = hex(&v.ci);
            let ada = hex(&v.ada);
            let adb = hex(&v.adb);
            let prs = v.password.as_bytes();

            assert_eq!(
                ::hex::encode(generator_string(prs, &ci, &sid)),
                v.generator_string,
                "generator_string: {ctx}"
            );
            assert_eq!(
                ::hex::encode(calculate_generator(prs, &ci, &sid).compress().to_bytes()),
                v.g,
                "generator: {ctx}"
            );

            let mut initiator = initiator_with(
                &v.password,
                &sid,
                &ci,
                &ada,
                &adb,
                clamp_scalar_le(hex32(&v.ya)),
            );
            assert_eq!(::hex::encode(initiator.msg_a), v.msg_a, "msgA: {ctx}");

            let responder = responder_with(
                &v.password,
                &sid,
                &initiator.msg_a,
                &ci,
                &ada,
                &adb,
                clamp_scalar_le(hex32(&v.yb)),
            )
            .unwrap();
            assert_eq!(::hex::encode(responder.msg_b), v.msg_b, "msgB: {ctx}");

            let k = scalar_mult_vfy(&clamp_scalar_le(hex32(&v.yb)), &initiator.msg_a).unwrap();
            assert_eq!(::hex::encode(k), v.k, "K: {ctx}");
            assert_eq!(
                ::hex::encode(transcript_ir(
                    &initiator.msg_a,
                    &ada,
                    &responder.msg_b,
                    &adb
                )),
                v.transcript_ir,
                "transcript_ir: {ctx}"
            );
            assert_eq!(
                ::hex::encode(*compute_isk(
                    &sid,
                    &k,
                    &initiator.msg_a,
                    &ada,
                    &responder.msg_b,
                    &adb
                )),
                v.raw_isk,
                "raw ISK: {ctx}"
            );
            assert_eq!(::hex::encode(responder.isk.as_bytes()), v.isk, "ISK: {ctx}");

            let initiator_key = initiator.finish(&responder.msg_b).unwrap();
            assert_eq!(
                ::hex::encode(initiator_key.as_bytes()),
                v.isk,
                "initiator ISK: {ctx}"
            );

            for (role, want) in [(Role::Client, &v.confirm_c), (Role::Server, &v.confirm_s)] {
                let tag = confirm_tag(
                    &responder.isk,
                    role,
                    &initiator.msg_a,
                    &responder.msg_b,
                    &v.pin,
                )
                .unwrap();
                assert_eq!(::hex::encode(tag), **want, "confirm {role:?}: {ctx}");
                assert!(verify_confirm(
                    &tag,
                    &responder.isk,
                    role,
                    &initiator.msg_a,
                    &responder.msg_b,
                    &v.pin
                )
                .unwrap());
            }
        }
    }

    // --- round trip and rejection -------------------------------------------

    #[test]
    fn same_password_agrees_and_a_different_one_does_not() {
        let sid = [7u8; 16];
        let mut a = initiator_with("123456", &sid, &[], &[], &[], random_scalar());
        let b = responder("123456", &sid, &a.msg_a, &[]).unwrap();
        assert_eq!(a.finish(&b.msg_b).unwrap().as_bytes(), b.isk.as_bytes());

        let mut a2 = initiator_with("123456", &sid, &[], &[], &[], random_scalar());
        let b2 = responder("654321", &sid, &a2.msg_a, &[]).unwrap();
        assert_ne!(a2.finish(&b2.msg_b).unwrap().as_bytes(), b2.isk.as_bytes());
    }

    #[test]
    fn the_channel_identifier_is_bound_into_the_key() {
        let sid = [9u8; 16];
        let pin_a = b"cert-pin-A".as_slice();
        let pin_b = b"cert-pin-B".as_slice();

        let mut a = initiator_with("123456", &sid, pin_a, &[], &[], random_scalar());
        let b = responder("123456", &sid, &a.msg_a, pin_a).unwrap();
        assert_eq!(a.finish(&b.msg_b).unwrap().as_bytes(), b.isk.as_bytes());

        // The man-in-the-middle case: the Client observed a different certificate
        // than Core serves, so the folded pins differ and the keys diverge.
        let mut a2 = initiator_with("123456", &sid, pin_a, &[], &[], random_scalar());
        let b2 = responder("123456", &sid, &a2.msg_a, pin_b).unwrap();
        assert_ne!(a2.finish(&b2.msg_b).unwrap().as_bytes(), b2.isk.as_bytes());

        // A webpki-mode Client folding nothing against a Core that folds its real
        // pin diverges the same way. Folding nothing agrees only when both sides
        // deliberately do it.
        let mut a3 = initiator_with("123456", &sid, &[], &[], &[], random_scalar());
        let b3 = responder("123456", &sid, &a3.msg_a, pin_a).unwrap();
        assert_ne!(a3.finish(&b3.msg_b).unwrap().as_bytes(), b3.isk.as_bytes());
    }

    #[test]
    fn the_responder_rejects_an_identity_or_misshapen_peer_element() {
        let sid = [1u8; 16];
        // 32 zero bytes are the canonical identity encoding, so they decode and
        // must be rejected explicitly.
        assert_eq!(
            responder("123456", &sid, &[0u8; 32], &[]).unwrap_err(),
            CpaceError::Identity
        );
        assert_eq!(
            responder("123456", &sid, &[0u8; 31], &[]).unwrap_err(),
            CpaceError::Decode
        );
        assert_eq!(
            responder("123456", &sid, &[0u8; 33], &[]).unwrap_err(),
            CpaceError::Decode
        );
        // A well-formed length that is not a valid encoding.
        assert_eq!(
            responder("123456", &sid, &[0xffu8; 32], &[]).unwrap_err(),
            CpaceError::Decode
        );
    }

    #[test]
    fn the_initiator_rejects_an_identity_or_misshapen_peer_element() {
        let sid = [2u8; 16];
        let mut a = initiator_with("123456", &sid, &[], &[], &[], random_scalar());
        assert_eq!(a.finish(&[0u8; 32]).unwrap_err(), CpaceError::Identity);
        let mut a2 = initiator_with("123456", &sid, &[], &[], &[], random_scalar());
        assert_eq!(a2.finish(&[0u8; 31]).unwrap_err(), CpaceError::Decode);
    }

    #[test]
    fn a_zero_scalar_is_never_sampled() {
        for _ in 0..64 {
            assert_ne!(sample_scalar().unwrap(), Scalar::ZERO);
        }
    }

    #[test]
    fn the_scalar_clamp_clears_the_top_four_bits() {
        let clamped = clamp_scalar_le([0xff; 32]);
        let bytes = clamped.to_bytes();
        assert_eq!(bytes[31], 0x0f);
        // Below the group order, so the reduction never fires and the clamped
        // bytes survive the round trip unchanged.
        let mut want = [0xffu8; 32];
        want[31] = 0x0f;
        assert_eq!(bytes, want);
    }

    // --- confirmation -------------------------------------------------------

    #[test]
    fn confirmation_round_trips_and_detects_every_tamper() {
        let isk = Isk(Zeroizing::new([7u8; 32]));
        let msg_a = [1u8; 32];
        let msg_b = [2u8; 32];
        let pin = "cert-pin-abc";

        let tag_c = confirm_tag(&isk, Role::Client, &msg_a, &msg_b, pin).unwrap();
        assert_eq!(tag_c.len(), 32);
        let ok = |tag: &[u8], role, a: &[u8], b: &[u8], p: &str| {
            verify_confirm(tag, &isk, role, a, b, p).unwrap()
        };
        assert!(ok(&tag_c, Role::Client, &msg_a, &msg_b, pin));

        // The role is part of the message, so a client tag never passes as a
        // server one.
        assert!(!ok(&tag_c, Role::Server, &msg_a, &msg_b, pin));

        let mut msg_a_bad = msg_a;
        msg_a_bad[0] ^= 0x01;
        assert!(!ok(&tag_c, Role::Client, &msg_a_bad, &msg_b, pin));

        let mut msg_b_bad = msg_b;
        msg_b_bad[5] ^= 0x80;
        assert!(!ok(&tag_c, Role::Client, &msg_a, &msg_b_bad, pin));

        assert!(!ok(&tag_c, Role::Client, &msg_a, &msg_b, "cert-pin-xyz"));

        // A cert-bound tag never verifies against the empty fold, in either
        // direction.
        assert!(!ok(&tag_c, Role::Client, &msg_a, &msg_b, ""));
        let tag_empty = confirm_tag(&isk, Role::Client, &msg_a, &msg_b, "").unwrap();
        assert!(ok(&tag_empty, Role::Client, &msg_a, &msg_b, ""));
        assert!(!ok(&tag_empty, Role::Client, &msg_a, &msg_b, pin));

        let mut tag_bad = tag_c;
        tag_bad[31] ^= 0x01;
        assert!(!ok(&tag_bad, Role::Client, &msg_a, &msg_b, pin));

        // A truncated tag is rejected on length, not accepted on a prefix match.
        assert!(!ok(&tag_c[..16], Role::Client, &msg_a, &msg_b, pin));

        let tag_s = confirm_tag(&isk, Role::Server, &msg_a, &msg_b, pin).unwrap();
        assert_ne!(tag_s, tag_c);
        assert!(ok(&tag_s, Role::Server, &msg_a, &msg_b, pin));
    }

    #[test]
    fn a_confirmation_field_past_the_two_byte_prefix_is_an_error() {
        let isk = Isk(Zeroizing::new([3u8; 32]));
        let long = "x".repeat(0x1_0000);
        assert_eq!(
            confirm_tag(&isk, Role::Client, &[1u8; 32], &[2u8; 32], &long).unwrap_err(),
            CpaceError::FieldTooLong
        );
        // One byte under the boundary still works.
        let edge = "x".repeat(0xffff);
        assert!(confirm_tag(&isk, Role::Client, &[1u8; 32], &[2u8; 32], &edge).is_ok());
    }

    // --- encoding helpers ---------------------------------------------------

    #[test]
    fn prepend_len_is_leb128_and_its_size_helper_agrees() {
        for len in [0usize, 1, 127, 128, 129, 255, 256, 16383, 16384] {
            let data = vec![0xabu8; len];
            let mut out = Vec::new();
            push_prepend_len(&data, &mut out);
            assert_eq!(out.len(), prepend_len_size(len), "len {len}");
            assert_eq!(&out[out.len() - len..], &data[..], "len {len}");
        }
        let mut out = Vec::new();
        push_prepend_len(&[0u8; 128], &mut out);
        assert_eq!(&out[..2], &[0x80, 0x01]);
        out.clear();
        push_prepend_len(&[0u8; 127], &mut out);
        assert_eq!(&out[..1], &[0x7f]);
    }

    #[test]
    fn the_zero_pad_clamps_at_zero_for_a_long_password() {
        // A long PRS drives len_zpad to 0, and the zero-length pad field still
        // contributes its own single length byte.
        let long = vec![b'x'; 300];
        let s = generator_string(&long, &[], &[]);
        let mut want = Vec::new();
        push_prepend_len(DSI, &mut want);
        push_prepend_len(&long, &mut want);
        push_prepend_len(&[], &mut want);
        push_prepend_len(&[], &mut want);
        push_prepend_len(&[], &mut want);
        assert_eq!(s, want);
    }

    #[test]
    fn the_isk_debug_never_prints_key_material() {
        let isk = Isk(Zeroizing::new([0xab; 32]));
        assert_eq!(format!("{isk:?}"), "Isk(<REDACTED>)");
        assert!(!format!("{isk:?}").contains("ab"));
    }
}
