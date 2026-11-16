//! Walking a folder item (never through links) and the reversible renaming of
//! its children ("also scramble names inside").

use super::{acl, win, Error, Result};
use crate::model::{ChildAcl, ChildAttributes, ChildRename, DirTimes, Extra};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// What one walk over a folder item collects before anything is changed.
#[derive(Default)]
pub struct Scan {
    pub size: u64,
    pub files: u64,
    /// Links (reparse points) found inside: renamed like files, never entered.
    pub links: u64,
    pub dir_times: Vec<DirTimes>,
    /// Files without the "archive" bit (Windows sets it on rename and on a
    /// permission change, so it has to be cleared again afterwards).
    pub attributes: Vec<ChildAttributes>,
    /// The rename plan, parents before their children.
    pub children: Vec<ChildRename>,
    /// (relative path, is folder, DACL) of every child that is not a link.
    pub acls: Vec<(String, bool, String)>,
}

pub struct ScanOptions<'a> {
    /// Record every child's DACL (Lock mode).
    pub acl: bool,
    /// Plan random names for every child.
    pub scramble: bool,
    /// Fail with `InUse` when a file inside is open in another program.
    pub probe: bool,
    /// Source of random names (injectable so that tests can force collisions).
    pub names: &'a mut dyn FnMut() -> String,
}

pub fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}\\{name}")
    }
}

fn under(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// Walks the item. `out` is filled as the walk goes, so that after an error
/// the caller still knows which folders were looked into.
pub fn scan(root: &Path, opts: &mut ScanOptions, out: &mut Scan) -> Result<()> {
    // (path with original names, path with scrambled names), both relative.
    let mut stack: Vec<(String, String)> = vec![(String::new(), String::new())];
    while let Some((orig_rel, scr_rel)) = stack.pop() {
        let dir = under(root, &orig_rel);
        let entries = win::list(&dir).map_err(|e| {
            if e.raw_os_error() == Some(win::ERROR_ACCESS_DENIED) {
                Error::NeedsElevation(format!(
                    "The folder \"{}\" inside cannot be read with this account.",
                    win::plain(&dir)
                ))
            } else {
                Error::Io(format!(
                    "The folder \"{}\" cannot be read: {e}",
                    win::plain(&dir)
                ))
            }
        })?;
        let mut taken: HashSet<String> = entries.iter().map(|e| e.name.to_lowercase()).collect();
        for e in entries {
            let rel = join_rel(&orig_rel, &e.name);
            let path = dir.join(&e.name);
            let link = e.stat.is_link();
            let scrambled = if opts.scramble {
                let name = loop {
                    let candidate = (opts.names)();
                    if !candidate.is_empty() && taken.insert(candidate.to_lowercase()) {
                        break candidate;
                    }
                };
                out.children.push(ChildRename {
                    dir: scr_rel.clone(),
                    original: e.name.clone(),
                    scrambled: name.clone(),
                });
                name
            } else {
                e.name.clone()
            };
            if link {
                out.links += 1;
                continue;
            }
            if opts.acl {
                let sddl = win::get_sddl(&path, false).map_err(|err| acl::acl_error(&path, err))?;
                if !win::can_write_dac(&path) {
                    return Err(acl::acl_error(
                        &path,
                        std::io::Error::from_raw_os_error(win::ERROR_ACCESS_DENIED),
                    ));
                }
                out.acls.push((rel.clone(), e.stat.is_dir(), sddl));
            }
            if e.stat.is_dir() {
                out.dir_times.push(DirTimes {
                    path: rel.clone(),
                    created: e.stat.created,
                    accessed: e.stat.accessed,
                    modified: e.stat.modified,
                });
                stack.push((rel, join_rel(&scr_rel, &scrambled)));
            } else {
                out.files += 1;
                out.size += e.stat.size;
                if e.stat.attributes & win::ATTR_ARCHIVE == 0 {
                    out.attributes.push(ChildAttributes {
                        path: rel.clone(),
                        attributes: e.stat.attributes,
                    });
                }
                if opts.probe && win::in_use(&path) {
                    return Err(Error::InUse(format!(
                        "\"{}\" is open in another program. Close it and try again.",
                        e.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Splits the recorded child DACLs into "what most have" and the exceptions.
pub fn fold_acls(acls: Vec<(String, bool, String)>, extra: &mut Extra) {
    let mut count: [HashMap<&str, usize>; 2] = [HashMap::new(), HashMap::new()];
    for (_, is_dir, sddl) in &acls {
        *count[*is_dir as usize].entry(sddl.as_str()).or_default() += 1;
    }
    let most = |m: &HashMap<&str, usize>| {
        m.iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(s, _)| s.to_string())
            .unwrap_or_default()
    };
    extra.acl_file = most(&count[0]);
    extra.acl_dir = most(&count[1]);
    extra.child_acls = acls
        .iter()
        .filter(|(_, is_dir, sddl)| {
            *sddl
                != if *is_dir {
                    extra.acl_dir.as_str()
                } else {
                    extra.acl_file.as_str()
                }
        })
        .map(|(path, _, sddl)| ChildAcl {
            path: path.clone(),
            sddl: sddl.clone(),
        })
        .collect();
}

fn rename_error(from: &Path, e: std::io::Error) -> Error {
    let name = from
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match e.raw_os_error() {
        Some(win::ERROR_SHARING_VIOLATION) | Some(win::ERROR_LOCK_VIOLATION) => Error::InUse(
            format!("\"{name}\" is open in another program. Close it and try again."),
        ),
        _ => Error::Io(format!("\"{name}\" could not be renamed: {e}")),
    }
}

/// Applies a rename plan below `root`. Safe to run again after an interruption:
/// entries that already carry their random name are skipped.
pub fn scramble(
    root: &Path,
    plan: &[ChildRename],
    after_each: &mut dyn FnMut(usize, usize) -> Result<()>,
) -> Result<()> {
    let total = plan.len();
    for (i, c) in plan.iter().enumerate() {
        let base = under(root, &c.dir);
        let from = base.join(&c.original);
        let to = base.join(&c.scrambled);
        if win::exists(&to) {
            if win::exists(&from) {
                return Err(Error::Io(format!(
                    "Two entries claim the same name inside \"{}\".",
                    win::plain(&base)
                )));
            }
        } else {
            if !win::exists(&from) {
                return Err(Error::NotFound(format!(
                    "\"{}\" disappeared while its folder was being protected.",
                    c.original
                )));
            }
            win::rename(&from, &to).map_err(|e| rename_error(&from, e))?;
        }
        after_each(i + 1, total)?;
    }
    Ok(())
}

/// Undoes a rename plan (deepest entries first). Safe to run on a plan that
/// was only partly applied or already partly undone.
pub fn unscramble(
    root: &Path,
    plan: &[ChildRename],
    after_each: &mut dyn FnMut(usize, usize) -> Result<()>,
) -> Result<()> {
    let total = plan.len();
    for (i, c) in plan.iter().rev().enumerate() {
        let base = under(root, &c.dir);
        let scrambled = base.join(&c.scrambled);
        if win::exists(&scrambled) {
            let original = base.join(&c.original);
            if win::exists(&original) {
                return Err(Error::Io(format!(
                    "Two entries claim the same name inside \"{}\".",
                    win::plain(&base)
                )));
            }
            win::rename(&scrambled, &original).map_err(|e| rename_error(&scrambled, e))?;
        }
        after_each(i + 1, total)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_common_dacl_becomes_the_default() {
        let mut extra = Extra::default();
        fold_acls(
            vec![
                ("a.txt".into(), false, "D:A".into()),
                ("b.txt".into(), false, "D:A".into()),
                ("c.txt".into(), false, "D:B".into()),
                ("sub".into(), true, "D:C".into()),
            ],
            &mut extra,
        );
        assert_eq!(extra.acl_file, "D:A");
        assert_eq!(extra.acl_dir, "D:C");
        assert_eq!(
            extra.child_acls,
            vec![ChildAcl {
                path: "c.txt".into(),
                sddl: "D:B".into()
            }]
        );
    }
}
