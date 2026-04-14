/// Build script: generates Rust FFI bindings from the SDRplay API C headers.
///
/// Requires the SDRplay API to be installed (from sdrplay.com/api/).
/// On macOS the headers land at /usr/local/include/sdrplay_api.h and
/// the dylib at /usr/local/lib/libsdrplay_api.dylib.
fn main() {
    let header = "/usr/local/include/sdrplay_api.h";

    // Tell cargo where to find the SDRplay dylib at link time.
    println!("cargo:rustc-link-search=/usr/local/lib");
    println!("cargo:rustc-link-lib=dylib=sdrplay_api");
    println!("cargo:rerun-if-changed={header}");

    let bindings = bindgen::Builder::default()
        .header(header)
        .allowlist_function("sdrplay_api_.*")
        .allowlist_type("sdrplay_api_.*")
        .allowlist_var("SDRPLAY_.*")
        // Derive common traits so generated types are ergonomic.
        .derive_debug(true)
        .derive_copy(true)
        .derive_default(true)
        .generate()
        .expect("failed to generate sdrplay_api bindings — is the SDRplay API installed?");

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("failed to write bindings");
}
