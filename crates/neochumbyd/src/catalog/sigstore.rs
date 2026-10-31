//! "Verified build", checked here: a GitHub artifact attestation for an
//! app's `.wasm`, as a Sigstore bundle, verified by neochumbyd itself rather
//! than taken from GitHub's word that one exists. It stays a label, shown to
//! the owner, never a requirement.
//!
//! A bundle verifies when all of these hold ([`verify`]):
//!
//! 1. **The log.** One of its transparency-log entries is from a Rekor log
//!    in the trusted root, and that log's signed promise (the SET) over the
//!    entry checks with the log's key. The entry is for this envelope: its
//!    payload's sha256, its signature and its certificate. The time the log
//!    gives is when the signing happened.
//! 2. **The certificate.** It chains to a Fulcio CA in the trusted root, was
//!    valid at that time, and is for code signing.
//! 3. **The signature.** The DSSE envelope's one signature checks with the
//!    certificate's key, over DSSE's pre-authentication encoding.
//! 4. **Who signed.** The certificate was issued for a GitHub Actions run
//!    (its OIDC issuer is `https://token.actions.githubusercontent.com`), of
//!    a workflow in the crate's own repository (its URI is
//!    `https://github.com/OWNER/REPO/.github/workflows/FILE@REF`), and its
//!    source repository is that repository.
//! 5. **What was signed.** The payload is an in-toto statement of SLSA
//!    provenance, and one of its subjects has the `.wasm`'s sha256.
//!
//! The trusted root is Sigstore's public-good instance (Fulcio and Rekor),
//! as `gh attestation trusted-root` gives it, kept in
//! `sigstore-trusted-root.json`: what attestations of public repositories
//! are made with. Not checked: certificate transparency (the SCTs), Rekor's
//! inclusion proof (the SET is the log's promise of it), and entries in
//! Rekor v2 logs, which carry no SET: an attestation logged only there is
//! unverified here, for now.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rustls_pki_types::{CertificateDer, SignatureVerificationAlgorithm, TrustAnchor, UnixTime};
use serde_json::Value;

/// Sigstore's public-good trusted root.
const PUBLIC_GOOD: &str = include_str!("sigstore-trusted-root.json");

/// Who GitHub Actions' OIDC tokens are from.
pub const GITHUB_ACTIONS: &str = "https://token.actions.githubusercontent.com";
/// What a DSSE envelope of an in-toto statement says it holds.
const IN_TOTO: &str = "application/vnd.in-toto+json";
/// SLSA provenance, which GitHub's build attestations are.
const SLSA_PROVENANCE: &str = "https://slsa.dev/provenance/v1";

/// Code signing (1.3.6.1.5.5.7.3.3), the extended key use Fulcio gives.
const CODE_SIGNING: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03];
/// Fulcio's extensions, 1.3.6.1.4.1.57264.1.N.
const FULCIO: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x83, 0xbf, 0x30, 0x01];
/// The OIDC issuer, raw (1) and as a DER UTF8String (8).
const ISSUER_V1: u8 = 1;
const ISSUER_V2: u8 = 8;
/// The source repository's URI (12).
const SOURCE_REPOSITORY: u8 = 12;
/// id-ecPublicKey and prime256v1, an ECDSA P-256 key's algorithm.
const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

/// The certificates' signature algorithms: Fulcio's are ECDSA.
static ALGS: &[&dyn SignatureVerificationAlgorithm] = &[
    webpki::ring::ECDSA_P256_SHA256,
    webpki::ring::ECDSA_P256_SHA384,
    webpki::ring::ECDSA_P384_SHA256,
    webpki::ring::ECDSA_P384_SHA384,
];

/// Seconds since 1970, from to (none: still).
#[derive(Clone, Copy, Debug)]
struct Valid {
    start: u64,
    end: Option<u64>,
}

impl Valid {
    fn at(&self, t: u64) -> bool {
        t >= self.start && self.end.is_none_or(|e| t <= e)
    }
}

/// A Fulcio CA: its root, and the intermediates under it.
struct Ca {
    root: CertificateDer<'static>,
    intermediates: Vec<CertificateDer<'static>>,
    valid: Valid,
}

/// A Rekor log with an ECDSA P-256 key: its id (the key's SPKI's sha256)
/// and its public point.
struct Log {
    id: Vec<u8>,
    point: Vec<u8>,
    valid: Valid,
}

/// Whom a bundle is checked against: Fulcio's CAs and Rekor's logs.
pub struct Trust {
    cas: Vec<Ca>,
    logs: Vec<Log>,
}

/// What the attestation must be of.
pub struct Expect<'a> {
    /// The crate's repository on GitHub.
    pub owner: &'a str,
    pub repo: &'a str,
    /// The `.wasm`'s sha256, in hex.
    pub sha256: &'a str,
}

/// A verified attestation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The workflow that signed it, in its repository, and its ref:
    /// `.github/workflows/release.yml@refs/tags/v1.0.0`.
    pub workflow: String,
    /// When, by the log (seconds since 1970).
    pub signed_at: u64,
}

impl Trust {
    /// Sigstore's public-good instance.
    pub fn public_good() -> Trust {
        Trust::from_json(PUBLIC_GOOD).expect("the trusted root kept with neochumbyd reads")
    }

    /// A trusted root in Sigstore's JSON (`application/vnd.dev.sigstore.trustedroot+json`).
    /// Logs with keys other than ECDSA P-256 are left out.
    pub fn from_json(json: &str) -> Result<Trust, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| format!("the trusted root: {e}"))?;
        let mut cas = Vec::new();
        for ca in v["certificateAuthorities"].as_array().into_iter().flatten() {
            let mut chain: Vec<CertificateDer<'static>> = Vec::new();
            for c in ca["certChain"]["certificates"].as_array().into_iter().flatten() {
                chain.push(CertificateDer::from(b64(&c["rawBytes"])?));
            }
            // Leaf-most first: the root is last.
            let Some(root) = chain.pop() else { continue };
            cas.push(Ca { root, intermediates: chain, valid: valid_for(&ca["validFor"])? });
        }
        let mut logs = Vec::new();
        for l in v["tlogs"].as_array().into_iter().flatten() {
            let key = &l["publicKey"];
            if key["keyDetails"].as_str() != Some("PKIX_ECDSA_P256_SHA_256") {
                continue;
            }
            let spki = b64(&key["rawBytes"])?;
            let point = p256_point(&spki).ok_or("the trusted root: a log's key isn't ECDSA P-256")?;
            let id = b64(&l["logId"]["keyId"])?;
            if id != sha256(&spki) {
                return Err("the trusted root: a log's id isn't its key's".into());
            }
            logs.push(Log { id, point, valid: valid_for(&key["validFor"])? });
        }
        if cas.is_empty() || logs.is_empty() {
            return Err("the trusted root has no CA or no log".into());
        }
        Ok(Trust { cas, logs })
    }
}

/// Checks `bundle` (a Sigstore bundle, as GitHub's attestations API gives
/// it) against `trust`, for `expect`.
pub fn verify(trust: &Trust, bundle: &Value, expect: &Expect) -> Result<Verified, String> {
    let media = bundle["mediaType"].as_str().unwrap_or_default();
    if !media.starts_with("application/vnd.dev.sigstore.bundle") {
        return Err(format!("not a Sigstore bundle ({media})"));
    }
    let vm = &bundle["verificationMaterial"];
    let cert = match &vm["certificate"]["rawBytes"] {
        Value::Null => b64(&vm["x509CertificateChain"]["certificates"][0]["rawBytes"])?,
        raw => b64(raw)?,
    };
    let env = &bundle["dsseEnvelope"];
    let payload_type = env["payloadType"].as_str().unwrap_or_default();
    if payload_type != IN_TOTO {
        return Err(format!("its envelope holds {payload_type}, not an in-toto statement"));
    }
    let payload = b64(&env["payload"])?;
    let sigs = env["signatures"].as_array().map(Vec::as_slice).unwrap_or_default();
    let [sig] = sigs else { return Err(format!("its envelope has {} signatures, not one", sigs.len())) };
    let sig = b64(&sig["sig"])?;

    // 1. The log: when it was signed.
    let signed_at = logged(trust, vm, &payload, &sig, &cert)?;

    // 2. The certificate, then 3. the signature.
    let der = CertificateDer::from(cert.as_slice());
    let leaf = webpki::EndEntityCert::try_from(&der).map_err(|e| format!("its certificate: {e:?}"))?;
    let time = UnixTime::since_unix_epoch(Duration::from_secs(signed_at));
    let mut chained = Err("no CA in the trusted root was valid then".to_string());
    for ca in trust.cas.iter().filter(|ca| ca.valid.at(signed_at)) {
        let anchor = webpki::anchor_from_trusted_cert(&ca.root).map_err(|e| format!("{e:?}"))?;
        let anchors: [TrustAnchor; 1] = [anchor];
        let usage = webpki::KeyUsage::required(CODE_SIGNING);
        match leaf.verify_for_usage(ALGS, &anchors, &ca.intermediates, time, usage, None, None) {
            Ok(_) => {
                chained = Ok(());
                break;
            }
            Err(e) => chained = Err(format!("{e:?}")),
        }
    }
    if let Err(e) = chained {
        return Err(format!("its certificate isn't from Sigstore's Fulcio, or wasn't valid when it was logged ({e})"));
    }
    let pae = pae(payload_type, &payload);
    if !ALGS.iter().any(|alg| leaf.verify_signature(*alg, &pae, &sig).is_ok()) {
        return Err("its signature doesn't check with its certificate".into());
    }

    // 4. Who signed it.
    let ext = Fulcio::read(&cert).ok_or("its certificate can't be read")?;
    if ext.issuer.as_deref() != Some(GITHUB_ACTIONS) {
        return Err(format!(
            "it wasn't signed by a GitHub Actions run (its issuer is {})",
            ext.issuer.as_deref().unwrap_or("not given")
        ));
    }
    let repo = format!("https://github.com/{}/{}", expect.owner, expect.repo);
    let prefix = format!("{repo}/.github/workflows/");
    let workflow = leaf
        .valid_uri_names()
        .find_map(|u| {
            let rest = strip_prefix_ignore_case(u, &prefix)?;
            rest.contains('@').then(|| format!(".github/workflows/{rest}"))
        })
        .ok_or_else(|| format!("it wasn't signed by a workflow of {repo}"))?;
    if !ext.source_repository.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&repo)) {
        return Err(format!("it wasn't built from {repo}"));
    }

    // 5. What it says was built.
    let st: Value = serde_json::from_slice(&payload).map_err(|e| format!("its statement: {e}"))?;
    if !st["_type"].as_str().is_some_and(|t| t.starts_with("https://in-toto.io/Statement/")) {
        return Err("its payload isn't an in-toto statement".into());
    }
    if st["predicateType"].as_str() != Some(SLSA_PROVENANCE) {
        return Err("it isn't build provenance".into());
    }
    let ours = st["subject"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| s["digest"]["sha256"].as_str().is_some_and(|d| d.eq_ignore_ascii_case(expect.sha256)));
    if !ours {
        return Err("it is for other files than this .wasm".into());
    }
    Ok(Verified { workflow, signed_at })
}

/// The time of the first of `vm`'s log entries that is from a log in
/// `trust`, promised by its SET, and for this envelope and certificate.
fn logged(trust: &Trust, vm: &Value, payload: &[u8], sig: &[u8], cert: &[u8]) -> Result<u64, String> {
    let mut why = "it is in no transparency log neochumbyd trusts".to_string();
    for e in vm["tlogEntries"].as_array().into_iter().flatten() {
        match entry(trust, e, payload, sig, cert) {
            Ok(t) => return Ok(t),
            Err(e) => why = e,
        }
    }
    Err(why)
}

fn entry(trust: &Trust, e: &Value, payload: &[u8], sig: &[u8], cert: &[u8]) -> Result<u64, String> {
    let id = b64(&e["logId"]["keyId"])?;
    let log = trust.logs.iter().find(|l| l.id == id).ok_or("its log isn't one neochumbyd trusts")?;
    let number = |v: &Value| -> Result<u64, String> {
        v.as_str().and_then(|s| s.parse().ok()).or(v.as_u64()).ok_or_else(|| "a log entry's number is missing".into())
    };
    let (time, index) = (number(&e["integratedTime"])?, number(&e["logIndex"])?);
    if !log.valid.at(time) {
        return Err("its log entry is from outside that log's time".into());
    }
    let body_b64 = e["canonicalizedBody"].as_str().ok_or("its log entry has no body")?;
    // The SET: the log's signature over the entry, as canonical JSON (keys
    // sorted, no spaces).
    let set = b64(&e["inclusionPromise"]["signedEntryTimestamp"])?;
    let signed = format!(
        r#"{{"body":"{body_b64}","integratedTime":{time},"logID":"{}","logIndex":{index}}}"#,
        neochumby_proto::hex(&id)
    );
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, &log.point)
        .verify(signed.as_bytes(), &set)
        .map_err(|_| "its log entry's promise doesn't check with the log's key")?;

    // The entry is this envelope's: the payload's hash, its signature and
    // its certificate.
    let body: Value =
        serde_json::from_slice(&b64(&Value::from(body_b64))?).map_err(|e| format!("its log entry: {e}"))?;
    if body["kind"].as_str() != Some("dsse") {
        return Err("its log entry isn't for a DSSE envelope".into());
    }
    let spec = &body["spec"];
    let hash = &spec["payloadHash"];
    if hash["algorithm"].as_str() != Some("sha256") || hash["value"].as_str() != Some(&hex(&sha256(payload))) {
        return Err("its log entry is for another payload".into());
    }
    let ours = spec["signatures"].as_array().into_iter().flatten().any(|s| {
        b64(&s["signature"]).is_ok_and(|s| s == sig)
            && b64(&s["verifier"]).is_ok_and(|pem| pem_der(&pem).is_some_and(|d| d == cert))
    });
    if !ours {
        return Err("its log entry is for another signature or certificate".into());
    }
    Ok(time)
}

/// DSSE's pre-authentication encoding of a payload.
fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut m = format!("DSSEv1 {} {payload_type} {} ", payload_type.len(), payload.len()).into_bytes();
    m.extend_from_slice(payload);
    m
}

/// What Fulcio's own extensions in a certificate say.
#[derive(Debug, Default)]
struct Fulcio {
    issuer: Option<String>,
    source_repository: Option<String>,
}

impl Fulcio {
    fn read(cert: &[u8]) -> Option<Fulcio> {
        let mut out = Fulcio::default();
        let mut c = Der(cert);
        let mut tbs = Der(c.expect(0x30)?).expect(0x30).map(Der)?;
        // version, serial, signature, issuer, validity, subject, key, then
        // the optional unique ids and the extensions.
        while let Some((tag, v)) = tbs.next() {
            if tag != 0xa3 {
                continue;
            }
            let mut exts = Der(Der(v).expect(0x30)?);
            while let Some(ext) = exts.expect(0x30) {
                let mut ext = Der(ext);
                let oid = ext.expect(0x06)?;
                let (mut tag, mut value) = ext.next()?;
                if tag == 0x01 {
                    (tag, value) = ext.next()?;
                }
                if tag != 0x04 {
                    return None;
                }
                let Some(n) = oid.strip_prefix(FULCIO) else { continue };
                let utf8 = || Der(value).expect(0x0c).and_then(|s| String::from_utf8(s.to_vec()).ok());
                match n {
                    [ISSUER_V2] => out.issuer = utf8(),
                    [ISSUER_V1] if out.issuer.is_none() => out.issuer = String::from_utf8(value.to_vec()).ok(),
                    [SOURCE_REPOSITORY] => out.source_repository = utf8(),
                    _ => {}
                }
            }
        }
        Some(out)
    }
}

/// A DER reader, enough for the certificate's extensions and a key.
struct Der<'a>(&'a [u8]);

impl<'a> Der<'a> {
    /// The next element: its tag and contents.
    fn next(&mut self) -> Option<(u8, &'a [u8])> {
        let (&tag, rest) = self.0.split_first()?;
        let (&first, mut rest) = rest.split_first()?;
        let len = match first {
            n @ 0..=0x7f => n as usize,
            0x81..=0x84 => {
                let (bytes, r) = rest.split_at_checked((first & 0x7f) as usize)?;
                rest = r;
                bytes.iter().fold(0usize, |n, &b| (n << 8) | b as usize)
            }
            _ => return None,
        };
        let (value, rest) = rest.split_at_checked(len)?;
        self.0 = rest;
        Some((tag, value))
    }

    /// The next element's contents, if it has tag `tag`.
    fn expect(&mut self, tag: u8) -> Option<&'a [u8]> {
        self.next().filter(|(t, _)| *t == tag).map(|(_, v)| v)
    }
}

/// The uncompressed point of an ECDSA P-256 SubjectPublicKeyInfo.
fn p256_point(spki: &[u8]) -> Option<Vec<u8>> {
    let mut s = Der(Der(spki).expect(0x30)?);
    let mut alg = Der(s.expect(0x30)?);
    if alg.expect(0x06)? != EC_PUBLIC_KEY || alg.expect(0x06)? != P256 {
        return None;
    }
    let bits = s.expect(0x03)?;
    let (0, point) = bits.split_first()? else { return None };
    (point.len() == 65 && point[0] == 4).then(|| point.to_vec())
}

/// The DER of the first certificate in PEM.
fn pem_der(pem: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(pem).ok()?;
    let body = s.split("-----BEGIN CERTIFICATE-----").nth(1)?.split("-----END CERTIFICATE-----").next()?;
    let b: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    B64.decode(b).ok()
}

/// `s` without `prefix`, which may differ in case (GitHub's owners and
/// repositories don't).
fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
}

fn b64(v: &Value) -> Result<Vec<u8>, String> {
    let s = v.as_str().ok_or("a value it must have is missing")?;
    B64.decode(s).map_err(|e| format!("bad base64: {e}"))
}

fn valid_for(v: &Value) -> Result<Valid, String> {
    let t = |k: &str| v[k].as_str().map(rfc3339).transpose();
    Ok(Valid { start: t("start")?.unwrap_or(0), end: t("end")? })
}

/// Seconds since 1970 of an RFC 3339 time in UTC (`2022-04-13T20:06:15Z`,
/// perhaps with a fraction).
fn rfc3339(s: &str) -> Result<u64, String> {
    let bad = || format!("{s}: not a time");
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return Err(bad());
    }
    if !s.ends_with('Z') {
        return Err(bad());
    }
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|d| d.parse::<i64>().ok()).ok_or_else(bad);
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from 1970 (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + hh * 3600 + mm * 60 + ss).map_err(|_| bad())
}

fn sha256(b: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(b).to_vec()
}

fn hex(b: &[u8]) -> String {
    neochumby_proto::hex(b)
}

#[cfg(test)]
pub(crate) mod tests;
