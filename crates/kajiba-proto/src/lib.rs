//! Generated types for the kajiba packages (0005, 0020). Source of truth: `proto/`.

// Nested as the package path: generated code reaches `google.protobuf` through `super::super`.
pub mod kajiba {
    pub mod v1 {
        tonic::include_proto!("kajiba.v1");
    }
}
pub use kajiba::v1;

pub mod google {
    // options.proto imports descriptor.proto, whose upstream doc comments trip this lint.
    #[allow(clippy::doc_overindented_list_items)]
    pub mod protobuf {
        tonic::include_proto!("google.protobuf");
    }
}

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("kajiba_descriptor");

use prost_reflect::DescriptorPool;
use std::collections::BTreeMap;

/// What a `kajiba.v1` method requires of its caller (0014, 0077).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Public,
    Scope(v1::Scope),
}

/// Method path (`/kajiba.v1.RepoService/GetRepo`) → access, for every `kajiba.v1` method.
/// Read from the encoded descriptor set: prost drops extension fields from `MethodOptions`.
pub fn method_access() -> Result<BTreeMap<String, Access>, String> {
    let pool = DescriptorPool::decode(FILE_DESCRIPTOR_SET).map_err(|e| e.to_string())?;
    access_table(&pool)
}

fn access_table(pool: &DescriptorPool) -> Result<BTreeMap<String, Access>, String> {
    let extension = |name: &str| {
        pool.get_extension_by_name(name)
            .ok_or_else(|| format!("{name} is not defined"))
    };
    let (scope, public) = (
        extension("kajiba.v1.scope")?,
        extension("kajiba.v1.public_access")?,
    );
    let mut table = BTreeMap::new();
    for service in pool.services().filter(|s| s.package_name() == "kajiba.v1") {
        for method in service.methods() {
            let path = format!("/{}/{}", service.full_name(), method.name());
            let options = method.options();
            let declared = match (
                options.has_extension(&scope),
                options.has_extension(&public),
            ) {
                (true, false) => options
                    .get_extension(&scope)
                    .as_enum_number()
                    .and_then(|n| v1::Scope::try_from(n).ok())
                    .filter(|s| *s != v1::Scope::Unspecified)
                    .map(Access::Scope),
                (false, true) => (options.get_extension(&public).as_bool() == Some(true))
                    .then_some(Access::Public),
                _ => None,
            };
            let access = declared.ok_or_else(|| {
                format!(
                    "{path} must declare exactly one of (kajiba.v1.scope) or (kajiba.v1.public_access) = true"
                )
            })?;
            table.insert(path, access);
        }
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost_reflect::prost_types::{
        FileDescriptorProto, MethodDescriptorProto, ServiceDescriptorProto,
    };

    #[test]
    fn every_public_method_declares_its_access() {
        let table = method_access().unwrap();
        let pool = DescriptorPool::decode(FILE_DESCRIPTOR_SET).unwrap();
        let methods = pool
            .services()
            .filter(|s| s.package_name() == "kajiba.v1")
            .map(|s| s.methods().len())
            .sum::<usize>();
        assert_eq!(table.len(), methods);
        assert_eq!(table["/kajiba.v1.HealthService/Check"], Access::Public);
        for method in [
            "GetRepo",
            "ListRepos",
            "GetTree",
            "GetBlob",
            "ListCommits",
            "GetCommitDiff",
        ] {
            assert_eq!(
                table[&format!("/kajiba.v1.RepoService/{method}")],
                Access::Scope(v1::Scope::RepoRead)
            );
        }
    }

    #[test]
    fn a_method_without_the_option_is_refused() {
        let mut pool = DescriptorPool::decode(FILE_DESCRIPTOR_SET).unwrap();
        pool.add_file_descriptor_proto(FileDescriptorProto {
            name: Some("kajiba/v1/fixture.proto".into()),
            package: Some("kajiba.v1".into()),
            dependency: vec!["kajiba/v1/health.proto".into()],
            service: vec![ServiceDescriptorProto {
                name: Some("FixtureService".into()),
                method: vec![MethodDescriptorProto {
                    name: Some("Bare".into()),
                    input_type: Some(".kajiba.v1.CheckRequest".into()),
                    output_type: Some(".kajiba.v1.CheckResponse".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            syntax: Some("proto3".into()),
            ..Default::default()
        })
        .unwrap();
        let error = access_table(&pool).unwrap_err();
        assert!(
            error.starts_with("/kajiba.v1.FixtureService/Bare "),
            "{error}"
        );
    }
}

pub mod admin {
    pub mod v1 {
        tonic::include_proto!("kajiba.admin.v1");
    }
}

pub mod hook {
    pub mod v1 {
        tonic::include_proto!("kajiba.hook.v1");
    }
}

pub mod git {
    pub mod v1 {
        tonic::include_proto!("kajiba.git.v1");
    }
}

pub mod gate {
    pub mod v1 {
        tonic::include_proto!("kajiba.gate.v1");
    }
}
