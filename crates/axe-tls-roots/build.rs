use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=AXE_EDITION_ROOT");

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let source_root = manifest_dir
        .ancestors()
        .nth(2)
        .expect("axe-tls-roots must be inside the AXE workspace");
    let edition_root = env::var_os("AXE_EDITION_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| source_root.to_path_buf());
    let additional_ca = edition_root.join("store/nix/assets/trusted_ca.pem");
    println!("cargo:rerun-if-changed={}", additional_ca.display());

    let bytes = if additional_ca.exists() {
        fs::read(&additional_ca).unwrap_or_else(|error| {
            panic!(
                "read additional CA bundle {}: {error}",
                additional_ca.display()
            )
        })
    } else {
        Vec::new()
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("additional-ca.pem");
    fs::write(&output, bytes)
        .unwrap_or_else(|error| panic!("write generated CA bundle {}: {error}", output.display()));
}
