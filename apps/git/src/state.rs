use kajiba_core::state::{StateLock, create_dir_all, real_dir, sync_dir};
use std::{fs, io, path::Path};

pub fn prepare(state_dir: &Path) -> Result<StateLock, String> {
    prepare_with_sync(state_dir, &mut sync_dir)
}

fn prepare_with_sync(
    state_dir: &Path,
    sync: &mut impl FnMut(&Path) -> io::Result<()>,
) -> Result<StateLock, String> {
    let directory = |path: &Path, sync: &mut _| {
        create_dir_all(path, sync)
            .and_then(|()| real_dir(path))
            .map_err(|err| format!("{}: {err}", path.display()))
    };
    directory(state_dir, sync)?;
    let lock = StateLock::acquire(&state_dir.join("kajiba-git.lock"))?;
    for name in ["repos", "tmp", "tmp/orphans"] {
        directory(&state_dir.join(name), sync)?;
    }
    let tmp = state_dir.join("tmp");
    let result = (|| -> io::Result<()> {
        for entry in fs::read_dir(&tmp)? {
            let entry = entry?;
            if entry.file_name() == "orphans" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        sync(&tmp)
    })();
    result.map_err(|err| format!("cleanup {}: {err}", tmp.display()))?;
    Ok(lock)
}

#[cfg(test)]
pub(crate) mod tests;
