use rcgen::{
    CertificateParams, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose, PKCS_ED25519,
    date_time_ymd,
};

pub(crate) fn quic_identity(subject: &str, usage: ExtendedKeyUsagePurpose) -> (String, String) {
    let mut parameters = CertificateParams::new(vec![subject.to_owned()])
        .expect("create test QUIC certificate parameters");
    parameters.not_before = date_time_ymd(2020, 1, 1);
    parameters.not_after = date_time_ymd(2120, 1, 1);
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![usage];

    let private_key = KeyPair::generate_for(&PKCS_ED25519).expect("generate test QUIC private key");
    let certificate = parameters
        .self_signed(&private_key)
        .expect("sign test QUIC certificate");

    (certificate.pem(), private_key.serialize_pem())
}
