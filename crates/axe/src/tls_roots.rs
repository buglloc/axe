use std::io::Cursor;
use std::sync::LazyLock;

use base64::Engine;
use rustls::pki_types::CertificateDer;

static ROOTS: LazyLock<Vec<CertificateDer<'static>>> = LazyLock::new(|| {
    let mut roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS.to_vec();
    let additional = crate::embedded::INPUTS.additional_ca_pem;
    if !additional.is_empty() {
        roots.extend(
            rustls_pemfile::certs(&mut Cursor::new(additional))
                .collect::<Result<Vec<_>, _>>()
                .expect("edition CA bundle was validated at build time"),
        );
    }
    roots
});

static PEM_BUNDLE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut pem = Vec::new();
    for certificate in ROOTS.iter() {
        pem.extend_from_slice(b"-----BEGIN CERTIFICATE-----\n");
        let encoded = base64::engine::general_purpose::STANDARD.encode(certificate.as_ref());
        for line in encoded.as_bytes().chunks(64) {
            pem.extend_from_slice(line);
            pem.push(b'\n');
        }
        pem.extend_from_slice(b"-----END CERTIFICATE-----\n");
    }
    pem
});

pub fn certificates() -> &'static [CertificateDer<'static>] {
    &ROOTS
}

pub fn certificates_der() -> Vec<Vec<u8>> {
    ROOTS
        .iter()
        .map(|certificate| certificate.to_vec())
        .collect()
}

pub fn pem_bundle() -> &'static [u8] {
    &PEM_BUNDLE
}
