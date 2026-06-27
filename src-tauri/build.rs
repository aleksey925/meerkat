fn main() {
    let hash = std::env::var("COMMIT_HASH").unwrap_or_default();
    println!("cargo:rustc-env=COMMIT_HASH={hash}");
    tauri_build::build()
}
