fn main() {
    println!("cargo:rerun-if-changed=src/long_double.c");
    let mut build = cc::Build::new();
    build
        .file("src/long_double.c")
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
