use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let brand = if env::var("PROFILE").as_deref() == Ok("debug") {
        "dev/blueprint-windows.ico"
    } else {
        "prod/t3-black-windows.ico"
    };
    let icon = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
        .join("../../../../assets")
        .join(brand);
    assert!(icon.is_file(), "Windows app icon exists");
    println!("cargo:rerun-if-changed={}", icon.display());
    let resource =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory")).join("t3-code.rc");
    let icon = icon
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    // GPUI's Windows platform loads the app icon from resource ID 1.
    fs::write(
        &resource,
        format!("#pragma code_page(65001)\n1 ICON \"{icon}\"\n"),
    )
    .expect("write Windows icon resource");
    embed_resource::compile(&resource, embed_resource::NONE)
        .manifest_required()
        .expect("compile Windows app icon; a Windows resource compiler is required");
}
