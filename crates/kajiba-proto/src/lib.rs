//! Generated types for the kajiba packages (0005, 0020). Source of truth: `proto/`.

pub mod v1 {
    tonic::include_proto!("kajiba.v1");
}

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("kajiba_descriptor");

pub mod admin {
    pub mod v1 {
        tonic::include_proto!("kajiba.admin.v1");
    }
}
