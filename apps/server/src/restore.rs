//! Offline restore validation; installation is a separate operation.
use std::path::Path;

// The offline command is wired in J8 task 6.
#[allow(dead_code)]
pub(crate) async fn validate(root: &Path) -> Result<crate::dump::Manifest, String> {
    crate::dump::verify(root).await
}
