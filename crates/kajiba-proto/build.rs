fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .file_descriptor_set_path(
            std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("kajiba_descriptor.bin"),
        )
        .build_server(true)
        .build_client(true)
        .compile_protos(&["../../proto/kajiba/v1/health.proto"], &["../../proto"])?;
    Ok(())
}
