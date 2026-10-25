fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .file_descriptor_set_path(
            std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("kajiba_descriptor.bin"),
        )
        .build_server(true)
        .build_client(true)
        // Generated in this crate rather than taken from prost-types (no new dependency, 0034).
        .compile_well_known_types(true)
        .compile_protos(
            &[
                "../../proto/kajiba/v1/health.proto",
                "../../proto/kajiba/v1/repo.proto",
                "google/protobuf/timestamp.proto",
            ],
            &["../../proto"],
        )?;
    tonic_build::configure().compile_protos(
        &[
            "../../proto/kajiba/admin/v1/admin.proto",
            "../../proto/kajiba/hook/v1/hook.proto",
            "../../proto/kajiba/git/v1/git.proto",
            "../../proto/kajiba/gate/v1/gate.proto",
        ],
        &["../../proto"],
    )?;
    Ok(())
}
