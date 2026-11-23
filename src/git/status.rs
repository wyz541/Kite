use std::path::{Path, PathBuf};

use crate::fs::VcsStatus;

#[derive(Clone, Debug)]
pub(crate) struct GitPathStatus {
    pub path: PathBuf,
    pub status: VcsStatus,
}

pub(crate) fn parse_git_status(output: &[u8], root: &Path) -> Vec<GitPathStatus> {
    let mut statuses = Vec::new();
    let mut records = output.split(|byte| *byte == 0);

    while let Some(record) = records.next() {
        if record.len() < 4 || record.get(2) != Some(&b' ') {
            continue;
        }
        let code = &record[..2];
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        if code.contains(&b'R') || code.contains(&b'C') {
            let _ = records.next();
        }
        if let Some(status) = git_status_from_code(code) {
            statuses.push(GitPathStatus {
                path: root.join(path),
                status,
            });
        }
    }

    statuses
}

pub(crate) fn git_status_from_code(code: &[u8]) -> Option<VcsStatus> {
    if code.contains(&b'?') {
        Some(VcsStatus::Untracked)
    } else if code.contains(&b'!') {
        Some(VcsStatus::Ignored)
    } else if code.contains(&b'U') {
        Some(VcsStatus::Conflicted)
    } else if code.contains(&b'R') {
        Some(VcsStatus::Renamed)
    } else if code.contains(&b'D') {
        Some(VcsStatus::Deleted)
    } else if code.contains(&b'A') || code.contains(&b'C') {
        Some(VcsStatus::Added)
    } else if code.contains(&b'M') || code.contains(&b'T') {
        Some(VcsStatus::Modified)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v1_changes_and_renames() {
        let root = Path::new("/repo");
        let output =
            b" M changed.rs\0?? new.rs\0!! ignored.log\0UU conflict.rs\0R  renamed.rs\0old.rs\0";
        let statuses = parse_git_status(output, root);
        assert_eq!(statuses.len(), 5);
        assert_eq!(statuses[0].status, VcsStatus::Modified);
        assert_eq!(statuses[1].status, VcsStatus::Untracked);
        assert_eq!(statuses[2].status, VcsStatus::Ignored);
        assert_eq!(statuses[3].status, VcsStatus::Conflicted);
        assert_eq!(statuses[4].status, VcsStatus::Renamed);
        assert!(statuses[0].path.ends_with("changed.rs"));
    }
}
