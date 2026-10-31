//! The verifier against a real attestation (one of this repository's
//! release assets, logged in Sigstore's public-good Rekor), and against
//! bundles made here, under a trusted root of the tests' own, for what no
//! real bundle shows: another issuer, another repository, a certificate
//! from no trusted CA, or one expired when it was logged.

use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, CustomExtension, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384, SanType, SigningKey, date_time_ymd,
};
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};
use serde_json::{Value, json};

use super::*;

/// An attestation GitHub keeps for one of this repository's release
/// assets: a classic image, built and attested by CI on main.
const REAL: &str = include_str!("neochumby-attestation.json");
const REAL_SHA256: &str = "953b430a25241b9719d832a7cb3b043b53e6b9b789be3b869ed125dfe1f1e890";

fn real() -> Value {
    serde_json::from_str(REAL).unwrap()
}

fn ours(sha256: &str) -> Expect<'_> {
    Expect { owner: "johnmerchant", repo: "neochumby", sha256 }
}

/// The payload of `b`, changed by `f`, put back.
fn with_payload(b: &mut Value, f: impl FnOnce(&mut Value)) {
    let p = B64.decode(b["dsseEnvelope"]["payload"].as_str().unwrap()).unwrap();
    let mut st: Value = serde_json::from_slice(&p).unwrap();
    f(&mut st);
    b["dsseEnvelope"]["payload"] = B64.encode(serde_json::to_vec(&st).unwrap()).into();
}

#[test]
fn the_kept_trusted_root_reads() {
    let t = Trust::public_good();
    assert!(t.cas.len() >= 2 && !t.logs.is_empty());
}

#[test]
fn a_real_attestation_verifies() {
    let v = verify(&Trust::public_good(), &real(), &ours(REAL_SHA256)).unwrap();
    assert_eq!(v.workflow, ".github/workflows/ci.yml@refs/heads/main");
    assert_eq!(v.signed_at, 1793357298);
    // Owners and repositories match whatever their case.
    let e = Expect { owner: "JohnMerchant", repo: "NeoChumby", sha256: &REAL_SHA256.to_uppercase() };
    assert!(verify(&Trust::public_good(), &real(), &e).is_ok());
}

#[test]
fn a_real_attestation_of_another_repository_or_file_doesnt() {
    let t = Trust::public_good();
    let other = Expect { owner: "someone", repo: "neochumby", sha256: REAL_SHA256 };
    let e = verify(&t, &real(), &other).unwrap_err();
    assert!(e.contains("workflow of https://github.com/someone/neochumby"), "{e}");
    let other = Expect { owner: "johnmerchant", repo: "neochumby-weather", sha256: REAL_SHA256 };
    assert!(verify(&t, &real(), &other).is_err());
    let e = verify(&t, &real(), &ours(&"0".repeat(64))).unwrap_err();
    assert!(e.contains("other files"), "{e}");
}

#[test]
fn a_tampered_real_attestation_doesnt() {
    let t = Trust::public_good();
    // Its statement made to name another file: the log has the old one.
    let mut b = real();
    with_payload(&mut b, |st| st["subject"][0]["digest"]["sha256"] = "0".repeat(64).into());
    let e = verify(&t, &b, &ours(&"0".repeat(64))).unwrap_err();
    assert!(e.contains("another payload"), "{e}");

    // Its signature changed.
    let mut b = real();
    let mut sig = B64.decode(b["dsseEnvelope"]["signatures"][0]["sig"].as_str().unwrap()).unwrap();
    let n = sig.len() - 3;
    sig[n] ^= 1;
    b["dsseEnvelope"]["signatures"][0]["sig"] = B64.encode(&sig).into();
    assert!(verify(&t, &b, &ours(REAL_SHA256)).is_err());

    // The log's time moved, so its promise no longer checks.
    let mut b = real();
    let e = &mut b["verificationMaterial"]["tlogEntries"][0];
    e["integratedTime"] = (e["integratedTime"].as_str().unwrap().parse::<u64>().unwrap() + 60).to_string().into();
    let err = verify(&t, &b, &ours(REAL_SHA256)).unwrap_err();
    assert!(err.contains("promise"), "{err}");

    // No log entry at all.
    let mut b = real();
    b["verificationMaterial"]["tlogEntries"] = json!([]);
    assert!(verify(&t, &b, &ours(REAL_SHA256)).is_err());

    // Not a bundle.
    assert!(verify(&t, &json!({"bundle": {}}), &ours(REAL_SHA256)).is_err());
}

/// A Sigstore of the tests' own: a CA (root and intermediate, as Fulcio's)
/// and a log key (as Rekor's).
pub(crate) struct Fake {
    root: rcgen::Certificate,
    intermediate: rcgen::Certificate,
    intermediate_issuer: Issuer<'static, KeyPair>,
    log: EcdsaKeyPair,
    log_spki: Vec<u8>,
}

/// How a test's bundle is made.
pub(crate) struct Make {
    pub(crate) issuer: &'static str,
    pub(crate) uri: String,
    pub(crate) source: String,
    pub(crate) sha256: String,
    /// Valid from, to (y, m, d), and logged at (seconds since 1970).
    pub(crate) valid: ((i32, u8, u8), (i32, u8, u8)),
    pub(crate) logged_at: u64,
}

impl Default for Make {
    fn default() -> Make {
        Make {
            issuer: GITHUB_ACTIONS,
            uri: "https://github.com/someone/neochumby-weather/.github/workflows/release.yml@refs/tags/v1.2.0".into(),
            source: "https://github.com/someone/neochumby-weather".into(),
            sha256: "ab".repeat(32),
            valid: ((2026, 1, 1), (2026, 1, 2)),
            // 2026-01-01T12:00:00Z
            logged_at: 1767268800,
        }
    }
}

fn utf8(s: &str) -> Vec<u8> {
    let mut v = vec![0x0c, s.len() as u8];
    v.extend_from_slice(s.as_bytes());
    v
}

fn ca_params(name: &str) -> CertificateParams {
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, name);
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p.not_before = date_time_ymd(2021, 1, 1);
    p.not_after = date_time_ymd(2031, 1, 1);
    p
}

impl Fake {
    pub(crate) fn new() -> Fake {
        let root_key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
        let root_params = ca_params("sigstore");
        let root = root_params.self_signed(&root_key).unwrap();
        let root_issuer = Issuer::new(root_params, root_key);
        let key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
        // Fulcio's intermediate is for code signing too.
        let mut params = ca_params("sigstore-intermediate");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::CodeSigning];
        let intermediate = params.signed_by(&key, &root_issuer).unwrap();
        let intermediate_issuer = Issuer::new(params, key);

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let log = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let mut log_spki = vec![0x30, 0x59, 0x30, 0x13, 0x06, 0x07];
        log_spki.extend_from_slice(EC_PUBLIC_KEY);
        log_spki.extend_from_slice(&[0x06, 0x08]);
        log_spki.extend_from_slice(P256);
        log_spki.extend_from_slice(&[0x03, 0x42, 0x00]);
        log_spki.extend_from_slice(log.public_key().as_ref());
        Fake { root, intermediate, intermediate_issuer, log, log_spki }
    }

    /// The trusted root, as Sigstore writes one.
    pub(crate) fn trusted_root(&self) -> String {
        json!({
            "mediaType": "application/vnd.dev.sigstore.trustedroot+json;version=0.1",
            "tlogs": [{
                "baseUrl": "https://rekor.example",
                "hashAlgorithm": "SHA2_256",
                "publicKey": {
                    "rawBytes": B64.encode(&self.log_spki),
                    "keyDetails": "PKIX_ECDSA_P256_SHA_256",
                    "validFor": {"start": "2021-01-01T00:00:00Z"}
                },
                "logId": {"keyId": B64.encode(sha256(&self.log_spki))}
            }],
            "certificateAuthorities": [{
                "subject": {"organization": "example", "commonName": "sigstore"},
                "uri": "https://fulcio.example",
                "certChain": {"certificates": [
                    {"rawBytes": B64.encode(self.intermediate.der())},
                    {"rawBytes": B64.encode(self.root.der())}
                ]},
                "validFor": {"start": "2021-01-01T00:00:00Z"}
            }]
        })
        .to_string()
    }

    pub(crate) fn trust(&self) -> Trust {
        Trust::from_json(&self.trusted_root()).unwrap()
    }

    /// A bundle as GitHub's attest action makes one, signed by a leaf
    /// certificate `issuer` issues.
    pub(crate) fn bundle_from(&self, m: &Make, issuer: &Issuer<'_, KeyPair>) -> Value {
        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
        p.subject_alt_names = vec![SanType::URI(m.uri.as_str().try_into().unwrap())];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::CodeSigning];
        let ((y, mo, d), (y2, mo2, d2)) = m.valid;
        p.not_before = date_time_ymd(y, mo, d);
        p.not_after = date_time_ymd(y2, mo2, d2);
        p.custom_extensions = vec![
            CustomExtension::from_oid_content(&[1, 3, 6, 1, 4, 1, 57264, 1, 8], utf8(m.issuer)),
            CustomExtension::from_oid_content(&[1, 3, 6, 1, 4, 1, 57264, 1, 12], utf8(&m.source)),
        ];
        let leaf = p.signed_by(&leaf_key, issuer).unwrap();
        let statement = json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{"name": "app.wasm", "digest": {"sha256": m.sha256}}],
            "predicateType": SLSA_PROVENANCE,
            "predicate": {}
        });
        let payload = serde_json::to_vec(&statement).unwrap();
        let sig = leaf_key.sign(&pae(IN_TOTO, &payload)).unwrap();
        let body = json!({
            "apiVersion": "0.0.1",
            "kind": "dsse",
            "spec": {
                "envelopeHash": {"algorithm": "sha256", "value": "00"},
                "payloadHash": {"algorithm": "sha256", "value": hex(&sha256(&payload))},
                "signatures": [{"signature": B64.encode(&sig), "verifier": B64.encode(leaf.pem())}]
            }
        });
        let body = B64.encode(serde_json::to_vec(&body).unwrap());
        let id = sha256(&self.log_spki);
        let signed =
            format!(r#"{{"body":"{body}","integratedTime":{},"logID":"{}","logIndex":7}}"#, m.logged_at, hex(&id));
        let set = self.log.sign(&ring::rand::SystemRandom::new(), signed.as_bytes()).unwrap();
        json!({
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "verificationMaterial": {
                "tlogEntries": [{
                    "logIndex": "7",
                    "logId": {"keyId": B64.encode(&id)},
                    "kindVersion": {"kind": "dsse", "version": "0.0.1"},
                    "integratedTime": m.logged_at.to_string(),
                    "inclusionPromise": {"signedEntryTimestamp": B64.encode(set.as_ref())},
                    "canonicalizedBody": body
                }],
                "certificate": {"rawBytes": B64.encode(leaf.der())}
            },
            "dsseEnvelope": {
                "payload": B64.encode(&payload),
                "payloadType": IN_TOTO,
                "signatures": [{"sig": B64.encode(&sig)}]
            }
        })
    }

    pub(crate) fn bundle(&self, m: &Make) -> Value {
        self.bundle_from(m, &self.intermediate_issuer)
    }
}

fn weather(sha256: &str) -> Expect<'_> {
    Expect { owner: "someone", repo: "neochumby-weather", sha256 }
}

#[test]
fn a_bundle_signed_as_github_signs_verifies() {
    let f = Fake::new();
    let m = Make::default();
    let v = verify(&f.trust(), &f.bundle(&m), &weather(&m.sha256)).unwrap();
    assert_eq!(v.workflow, ".github/workflows/release.yml@refs/tags/v1.2.0");
    assert_eq!(v.signed_at, m.logged_at);
    // Not under the real root.
    assert!(verify(&Trust::public_good(), &f.bundle(&m), &weather(&m.sha256)).is_err());
}

#[test]
fn a_bundle_from_another_identity_doesnt() {
    let f = Fake::new();
    let t = f.trust();
    let m = Make { issuer: "https://accounts.example.com", ..Make::default() };
    let e = verify(&t, &f.bundle(&m), &weather(&m.sha256)).unwrap_err();
    assert!(e.contains("GitHub Actions"), "{e}");

    // A workflow of another repository, say a reusable one, or a fork's.
    let m =
        Make { uri: "https://github.com/mallory/x/.github/workflows/w.yml@refs/heads/main".into(), ..Make::default() };
    let e = verify(&t, &f.bundle(&m), &weather(&m.sha256)).unwrap_err();
    assert!(e.contains("workflow of https://github.com/someone/neochumby-weather"), "{e}");

    // Its workflow's, but built from another repository.
    let m = Make { source: "https://github.com/mallory/neochumby-weather".into(), ..Make::default() };
    let e = verify(&t, &f.bundle(&m), &weather(&m.sha256)).unwrap_err();
    assert!(e.contains("built from"), "{e}");

    // A URI that is the repository's, but no workflow's.
    let m = Make { uri: "https://github.com/someone/neochumby-weather".into(), ..Make::default() };
    assert!(verify(&t, &f.bundle(&m), &weather(&m.sha256)).is_err());
}

#[test]
fn a_certificate_from_no_trusted_ca_or_expired_doesnt() {
    let f = Fake::new();
    let other = Fake::new();
    let m = Make::default();
    // Logged in the trusted log, but its certificate is from another CA.
    let b = f.bundle_from(&m, &other.intermediate_issuer);
    let e = verify(&f.trust(), &b, &weather(&m.sha256)).unwrap_err();
    assert!(e.contains("Fulcio"), "{e}");
    // Signed by our CA, but logged in another log.
    let b = other.bundle(&m);
    let mut t = f.trust();
    t.cas = other.trust().cas;
    let e = verify(&t, &b, &weather(&m.sha256)).unwrap_err();
    assert!(e.contains("no transparency log") || e.contains("isn't one neochumbyd trusts"), "{e}");
    // Logged after its certificate expired.
    let m = Make { logged_at: 1767268800 + 3 * 86400, ..Make::default() };
    let e = verify(&f.trust(), &f.bundle(&m), &weather(&m.sha256)).unwrap_err();
    assert!(e.contains("wasn't valid"), "{e}");
}

#[test]
fn a_tampered_bundle_doesnt() {
    let f = Fake::new();
    let t = f.trust();
    let m = Make::default();
    // Its subject changed to another file's.
    let mut b = f.bundle(&m);
    with_payload(&mut b, |st| st["subject"][0]["digest"]["sha256"] = "cd".repeat(32).into());
    assert!(verify(&t, &b, &weather(&"cd".repeat(32))).is_err());
    // Not provenance.
    let mut b = f.bundle(&m);
    with_payload(&mut b, |st| st["predicateType"] = "https://example.com/other".into());
    assert!(verify(&t, &b, &weather(&m.sha256)).is_err());
    // Two signatures.
    let mut b = f.bundle(&m);
    let s = b["dsseEnvelope"]["signatures"][0].clone();
    b["dsseEnvelope"]["signatures"] = json!([s.clone(), s]);
    assert!(verify(&t, &b, &weather(&m.sha256)).unwrap_err().contains("not one"));
}

#[test]
fn times() {
    assert_eq!(rfc3339("1970-01-01T00:00:00Z"), Ok(0));
    assert_eq!(rfc3339("2022-04-13T20:06:15Z"), Ok(1649880375));
    assert_eq!(rfc3339("2022-12-31T23:59:59.999Z"), Ok(1672531199));
    assert!(rfc3339("2022-12-31 23:59:59").is_err());
}

#[test]
fn der_lengths() {
    // Long form, and a length past the end.
    let mut long = vec![0x04, 0x81, 0x80];
    long.extend_from_slice(&[7; 0x80]);
    assert_eq!(Der(&long).next().map(|(t, v)| (t, v.len())), Some((0x04, 0x80)));
    assert_eq!(Der(&[0x04, 0x05, 1, 2]).next(), None);
    assert_eq!(Der(&[0x04, 0x80]).next(), None, "no indefinite lengths");
}
