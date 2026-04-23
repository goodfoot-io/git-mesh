use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let package_json_path = manifest_dir.join("package.json");
    println!("cargo:rerun-if-changed={}", package_json_path.display());

    let package_json = fs::read_to_string(&package_json_path).expect("read package.json");
    let parsed: serde_json::Value =
        serde_json::from_str(&package_json).expect("parse package.json");
    let version = parsed
        .get("version")
        .and_then(serde_json::Value::as_str)
        .expect("package.json version");

    println!("cargo:rustc-env=GIT_MESH_VERSION={version}");
}
