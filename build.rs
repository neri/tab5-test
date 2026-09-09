fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FONT_DROM_DIRECT");
    if std::env::var_os("CARGO_FEATURE_FONT_DROM_DIRECT").is_some() {
        println!("cargo:rustc-link-arg=--defsym=__font_drom_direct=1");
    }
}
