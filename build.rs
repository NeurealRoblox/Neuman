//! Conditional Tauri build integration for the optional desktop feature.

use std::{env, fs, path::PathBuf};

const BOOTSTRAP_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5, 0x1c, 0x0c,
    0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0xf8, 0x0f, 0x00,
    0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

/// Keep the bootstrap repository text-only while still supplying the Windows
/// resource compiler with a valid icon. Release packaging replaces this tiny
/// embedded icon with the project's signed brand asset.
#[cfg(target_os = "windows")]
fn bootstrap_icon() -> PathBuf {
    let mut ico = vec![0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0];
    let png_length = u32::try_from(BOOTSTRAP_PNG.len()).expect("bootstrap PNG length fits ICO");
    ico.extend_from_slice(&png_length.to_le_bytes());
    ico.extend_from_slice(&22_u32.to_le_bytes());
    ico.extend_from_slice(BOOTSTRAP_PNG);

    let path = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"))
        .join("neuman-bootstrap.ico");
    fs::write(&path, ico).expect("write bootstrap Windows icon");
    path
}

fn main() {
    println!("cargo:rerun-if-env-changed=NEUMAN_ROBLOX_OAUTH_CLIENT_ID");
    if env::var_os("CARGO_FEATURE_DESKTOP").is_none() {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));
    let png_path = out_dir.join("neuman-bootstrap.png");
    fs::write(&png_path, BOOTSTRAP_PNG).expect("write bootstrap application icon");

    let mut icon_paths = vec![png_path];
    let mut attributes = tauri_build::Attributes::new();
    #[cfg(target_os = "windows")]
    {
        let ico_path = bootstrap_icon();
        icon_paths.push(ico_path.clone());
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(ico_path));
    }

    let icons_json = icon_paths
        .iter()
        .map(|path| format!("\"{}\"", path.display().to_string().replace('\\', "/")))
        .collect::<Vec<_>>()
        .join(",");
    let config_override = format!("{{\"bundle\":{{\"icon\":[{icons_json}]}}}}");
    // Tauri's proc macro runs after this script and must see the generated
    // asset override. tauri-build receives the Windows path through Attributes.
    println!("cargo:rustc-env=TAURI_CONFIG={config_override}");

    tauri_build::try_build(attributes).expect("build Tauri desktop resources");
}
