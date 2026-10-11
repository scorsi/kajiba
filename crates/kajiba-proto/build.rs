fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["../../proto/kajiba/v1/health.proto"], &["../../proto"])?;
    Ok(())
}
