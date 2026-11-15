fn main() {
    let source = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
    )
    .join("src/long_double.c");
    println!("cargo:rerun-if-changed=src/long_double.c");
    // cargo-zigbuild's compiler version probe can write a.o in the working directory.
    std::env::set_current_dir(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .expect("enter Cargo build output directory");
    let mut build = cc::Build::new();
    build
        .file(source)
        .std("c17")
        .flag_if_supported("-fno-fast-math");
    // Nix's FORTIFY_SOURCE requires optimization in debug builds too.
    if std::env::var("OPT_LEVEL").as_deref() == Ok("0") {
        build.opt_level(1);
    }
    build.compile("axe_printf_long_double");
    // musl includes math in libc; Rust's self-contained sysroot has no libm.a.
    if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() != Ok("apple")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("musl")
    {
        println!("cargo:rustc-link-lib=m");
    }
}
