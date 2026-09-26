fn main() {
    println!("cargo::rerun-if-changed=proto/rpc.proto");
    println!("cargo::rerun-if-changed=proto/seccomp.proto");

    prost_build::Config::new()
        .compile_protos(&["proto/rpc.proto", "proto/seccomp.proto"], &["proto"])
        .expect("compile Porto protocol definitions");
}
