//! End-to-end tests of the engine. Everything happens inside `veilforge-*`
//! sandbox folders in the temp folder, removed again by a drop guard.

use super::cleanup::SandboxGuard;
use super::*;
use crate::model::Mode;
use std::collections::BTreeMap;
use std::os::windows::fs::OpenOptionsExt;

const PW: &str = "correct horse battery staple";

fn quiet() -> impl FnMut(Progress) {
    |_| {}
}

fn sandbox(tag: &str) -> (SandboxGuard, Env) {
    let guard = SandboxGuard::new(tag);
    let env = Env::sandbox(guard.path());
    std::fs::create_dir_all(guard.path().join("work")).unwrap();
    (guard, env)
}

fn opts(mode: Mode) -> ProtectOptions {
    ProtectOptions {
        mode,
        ask_hidden: true,
        scramble: true,
        group: String::new(),
    }
}

/// A FILETIME in 2019 plus `n` steps of about 2.8 hours.
fn old_time(n: u64) -> u64 {
    132_000_000_000_000_000 + n * 100_000_000_000
}

/// What the tests compare before and after: everything `release` promises.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snap {
    sddl: String,
    attributes: u32,
    created: u64,
    accessed: u64,
    modified: u64,
}

fn snap(path: &Path) -> Snap {
    let st = win::stat(path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
    Snap {
        sddl: win::get_sddl(path, true).unwrap(),
        attributes: st.attributes,
        created: st.created,
        accessed: st.accessed,
        modified: st.modified,
    }
}

/// A test item with known content: relative path -> bytes (`None` = folder).
struct Fixture {
    root: PathBuf,
    entries: BTreeMap<String, Option<Vec<u8>>>,
}

impl Fixture {
    fn file(dir: &Path, name: &str, content: &[u8]) -> Fixture {
        let root = dir.join(name);
        std::fs::write(win::long(&root), content).unwrap();
        win::set_times(&root, old_time(1), old_time(2), old_time(3)).unwrap();
        let mut entries = BTreeMap::new();
        entries.insert(String::new(), Some(content.to_vec()));
        Fixture { root, entries }
    }

    /// A folder tree with nested folders, repeated names, a name that looks
    /// like a random name, special attributes and unusual permissions.
    fn tree(dir: &Path, name: &str) -> Fixture {
        let root = dir.join(name);
        let mut entries: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
        entries.insert(String::new(), None);
        let mut big = vec![0u8; 70_000];
        crate::secret::random_bytes(&mut big);
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("a.txt", b"alpha".to_vec()),
            ("Budget 2031.docx", b"not really a docx".to_vec()),
            ("0123456789abcdef", b"looks like a random name".to_vec()),
            ("sub\\a.txt", b"another a".to_vec()),
            ("sub\\deep\\x.bin", big),
            ("sub\\deep\\readonly hidden.ini", b"[x]\r\ny=1\r\n".to_vec()),
            (
                "Sub two\\\u{fc}n\u{ef}c\u{f6}de \u{2603}.txt",
                "snow".as_bytes().to_vec(),
            ),
            ("Sub two\\own rules.txt", b"explicit acl".to_vec()),
        ];
        for d in ["sub", "sub\\deep", "sub\\deep\\empty", "Sub two", "closed"] {
            std::fs::create_dir_all(win::long(&root.join(d))).unwrap();
            entries.insert(d.to_string(), None);
        }
        for (rel, content) in files {
            std::fs::write(win::long(&root.join(rel)), &content).unwrap();
            entries.insert(rel.to_string(), Some(content));
        }
        std::fs::write(win::long(&root.join("closed\\inside.txt")), b"in").unwrap();
        entries.insert("closed\\inside.txt".into(), Some(b"in".to_vec()));
        // Unusual permissions: an explicit unprotected DACL on a file and a
        // protected one on a folder.
        let me = win::user_sid();
        win::set_dacl(
            &root.join("Sub two\\own rules.txt"),
            &format!("D:(A;;FA;;;{me})(A;;FR;;;WD)"),
        )
        .unwrap();
        win::set_dacl(
            &root.join("closed"),
            &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)"),
        )
        .unwrap();
        win::set_attributes(
            &root.join("sub\\deep\\readonly hidden.ini"),
            win::ATTR_READONLY | win::ATTR_HIDDEN,
        )
        .unwrap();
        // Distinct old timestamps everywhere, the root last.
        let rels: Vec<String> = entries.keys().rev().cloned().collect();
        for (n, rel) in rels.iter().enumerate() {
            let n = n as u64 * 3;
            win::set_times(
                &root.join(rel),
                old_time(n),
                old_time(n + 1),
                old_time(n + 2),
            )
            .unwrap();
        }
        Fixture { root, entries }
    }

    fn path(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }

    /// Metadata of every entry, by path, without listing or reading anything.
    /// Folders are read before anything below them, and afterwards their
    /// timestamps are put back (deepest first): on NTFS, reaching a child
    /// through a folder moves the folder's last-access time, and the snapshot
    /// must not leave a trace of its own.
    fn snapshot(&self) -> BTreeMap<String, Snap> {
        let all: BTreeMap<String, Snap> = self
            .entries
            .keys()
            .map(|rel| (rel.clone(), snap(&self.path(rel))))
            .collect();
        for (rel, s) in all.iter().rev() {
            if self.entries[rel].is_none() {
                win::set_times(&self.path(rel), s.created, s.accessed, s.modified).unwrap();
            }
        }
        all
    }

    /// After the test itself reached into the fixture: puts the dates back
    /// and returns `before` unchanged, for the next comparison.
    fn snapshot_after_own_read(&self, before: &BTreeMap<String, Snap>) -> BTreeMap<String, Snap> {
        for (rel, s) in before.iter().rev() {
            win::set_times(&self.path(rel), s.created, s.accessed, s.modified).unwrap();
        }
        before.clone()
    }

    /// Asserts that metadata equals `before`, then that names and bytes do.
    fn assert_restored(&self, before: &BTreeMap<String, Snap>) {
        let after = self.snapshot();
        for (rel, was) in before {
            assert_eq!(&after[rel], was, "metadata of {rel:?}");
        }
        self.assert_content();
        // Reading the content moved last-access times. That is this check's
        // own trace, not the engine's: put the dates back (deepest first) so
        // that a later protect records what the fixture started with.
        for (rel, s) in before.iter().rev() {
            win::set_times(&self.path(rel), s.created, s.accessed, s.modified).unwrap();
        }
    }

    fn assert_content(&self) {
        for (rel, content) in &self.entries {
            let p = self.path(rel);
            match content {
                Some(bytes) => assert_eq!(
                    &std::fs::read(win::long(&p)).unwrap_or_else(|e| panic!("{rel}: {e}")),
                    bytes,
                    "bytes of {rel:?}"
                ),
                None => assert!(win::is_dir(&p), "{rel:?} should be a folder"),
            }
        }
        // Nothing extra, nothing renamed.
        if self.entries[""].is_none() {
            let mut found = Vec::new();
            walk(&self.root, &self.root, &mut found);
            let mut expected: Vec<String> = self
                .entries
                .keys()
                .filter(|k| !k.is_empty())
                .cloned()
                .collect();
            found.sort();
            expected.sort();
            assert_eq!(found, expected);
        }
    }
}

/// Relative paths of everything listable below `dir`.
fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = win::list(dir) else {
        return;
    };
    for e in entries {
        let p = dir.join(&e.name);
        out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
        if e.stat.is_dir() && !e.stat.is_link() {
            walk(base, &p, out);
        }
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    let utf8 = needle.as_bytes();
    let utf16: Vec<u8> = needle
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes())
        .collect();
    let lower: Vec<u8> = haystack.to_ascii_lowercase();
    let n8 = utf8.to_ascii_lowercase();
    let n16 = utf16.to_ascii_lowercase();
    lower.windows(n8.len()).any(|w| w == n8) || lower.windows(n16.len()).any(|w| w == n16)
}

/// Asserts that no file in the data folder mentions any of `words`.
fn assert_data_dir_is_opaque(env: &Env, words: &[&str]) {
    let mut seen = 0;
    for e in std::fs::read_dir(&env.data_dir).unwrap() {
        let p = e.unwrap().path();
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        seen += 1;
        for w in words {
            assert!(!contains(&bytes, w), "{} mentions {w:?}", p.display());
        }
    }
    assert!(seen > 0);
}

fn denied<T>(r: std::io::Result<T>) -> bool {
    matches!(r, Err(e) if e.raw_os_error() == Some(win::ERROR_ACCESS_DENIED))
}

fn hold_dir(env: &Env) -> PathBuf {
    env.hold_root.clone().unwrap()
}

fn is_random_name(name: &str) -> bool {
    name.len() == 16 && name.chars().all(|c| c.is_ascii_hexdigit()) && !name.contains('.')
}

// --------------------------------------------------------------------------------------
// Lock
// --------------------------------------------------------------------------------------

#[test]
fn lock_file_round_trip_and_access_is_denied() {
    let (sb, env) = sandbox("lockfile");
    let fx = Fixture::file(&sb.path().join("work"), "Diary 2031.txt", b"dear diary");
    win::set_attributes(&fx.root, 0x20 | win::ATTR_HIDDEN).unwrap();
    let before = fx.snapshot();
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let id = v
        .protect(&fx.root, &opts(Mode::Lock), &mut quiet())
        .unwrap();
    assert_eq!(v.state(&id), State::Locked);
    let it = v.item(&id).unwrap().clone();
    assert_eq!(it.name, "Diary 2031.txt");
    assert_eq!(
        (it.kind, it.mode, it.size, it.files),
        (Kind::File, Mode::Lock, 10, 1)
    );
    assert!(it.hold_path.is_none() && it.standin.is_none() && !it.reduced);
    assert_eq!(it.saved.sddl, before[""].sddl);

    // Still in place, but closed to every ordinary access.
    assert!(win::exists(&fx.root));
    assert!(denied(std::fs::File::open(&fx.root)));
    assert!(denied(std::fs::read(&fx.root)));
    assert!(denied(
        std::fs::OpenOptions::new().write(true).open(&fx.root)
    ));
    assert!(denied(
        std::fs::OpenOptions::new().append(true).open(&fx.root)
    ));
    assert!(denied(std::fs::copy(
        &fx.root,
        sb.path().join("work\\copy.txt")
    )));
    assert!(denied(std::fs::remove_file(&fx.root)));
    assert!(denied(win::set_attributes(&fx.root, win::ATTR_NORMAL)));
    assert!(denied(win::set_times(&fx.root, 1, 2, 3)));
    assert!(win::exists(&fx.root));
    assert_eq!(win::get_sddl(&fx.root, false).unwrap(), acl::lock_sddl());

    let back = v.release(&id, &mut quiet()).unwrap();
    assert_eq!(back, fx.root);
    assert!(v.items().is_empty());
    fx.assert_restored(&before);
    // The recovery copy is gone and the holding folder can be removed.
    assert!(hold::list(&hold_dir(&env)).unwrap().is_empty());
    assert_eq!(v.remove_empty_holds(), 1);
    assert!(!win::exists(&hold_dir(&env)));
}

/// What an ordinary program of the same user can still do to a locked item
/// through the parent folder (NTFS lets the parent's "delete child" right
/// stand in for the item's own DELETE right). The asserts pin down what was
/// measured on NTFS; see the report in `docs` of the engine author.
#[test]
fn lock_measured_limits_delete_and_rename_through_the_parent() {
    let (sb, env) = sandbox("locklimits");
    let work = sb.path().join("work");
    let file = Fixture::file(&work, "single.txt", b"single file");
    let tree = Fixture::tree(&work, "folder");
    let empty = work.join("empty folder");
    std::fs::create_dir(&empty).unwrap();
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let id_file = v
        .protect(&file.root, &opts(Mode::Lock), &mut quiet())
        .unwrap();
    let id_tree = v
        .protect(&tree.root, &opts(Mode::Lock), &mut quiet())
        .unwrap();
    let id_empty = v.protect(&empty, &opts(Mode::Lock), &mut quiet()).unwrap();

    // Deleting: refused for all three (the read-only attribute set by the lock
    // blocks what the parent's delete-child right would otherwise allow, and
    // the attribute cannot be cleared because write-attributes is denied).
    let del_file = std::fs::remove_file(&file.root);
    let del_tree = std::fs::remove_dir_all(&tree.root);
    let del_tree_plain = std::fs::remove_dir(&tree.root);
    let del_empty = std::fs::remove_dir(&empty);
    println!("MEASURED delete locked file: {del_file:?}");
    println!("MEASURED delete locked folder (recursive): {del_tree:?}");
    println!("MEASURED delete locked folder (plain): {del_tree_plain:?}");
    println!("MEASURED delete locked empty folder: {del_empty:?}");
    assert!(del_file.is_err() && win::exists(&file.root));
    assert!(del_tree.is_err() && del_tree_plain.is_err() && win::exists(&tree.root));
    assert!(del_empty.is_err() && win::exists(&empty));

    // Renaming / moving within the volume through the parent.
    let moved_file = work.join("single renamed.txt");
    let moved_tree = work.join("folder renamed");
    let ren_file = win::rename(&file.root, &moved_file);
    let ren_tree = win::rename(&tree.root, &moved_tree);
    println!("MEASURED rename locked file: {ren_file:?}");
    println!("MEASURED rename locked folder: {ren_tree:?}");
    // Whatever the outcome, the content stays unreadable and nothing is lost.
    for (r, from, to, id) in [
        (&ren_file, &file.root, &moved_file, &id_file),
        (&ren_tree, &tree.root, &moved_tree, &id_tree),
    ] {
        if r.is_ok() {
            assert!(denied(std::fs::File::open(to)) || denied(std::fs::read_dir(to)));
            assert_eq!(v.state(id), State::Missing);
            win::rename(to, from).unwrap();
        }
        assert_eq!(v.state(id), State::Locked);
    }
    // Replacing a locked file by renaming another file over it is refused.
    let other = work.join("other.txt");
    std::fs::write(&other, b"other").unwrap();
    let replace = std::fs::rename(&other, &file.root);
    println!("MEASURED replace locked file by rename-over: {replace:?}");
    assert!(replace.is_err());

    for id in [id_file, id_tree, id_empty] {
        v.release(&id, &mut quiet()).unwrap();
    }
    file.assert_content();
    tree.assert_content();
}

#[test]
fn lock_folder_round_trip_and_children_are_closed() {
    let (sb, env) = sandbox("lockdir");
    let fx = Fixture::tree(&sb.path().join("work"), "Private papers");
    let before = fx.snapshot();
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let mut seen = Vec::new();
    let id = v
        .protect(&fx.root, &opts(Mode::Lock), &mut |p| seen.push(p.what))
        .unwrap();
    assert!(seen.iter().any(|w| w.contains("Locking")));
    let it = v.item(&id).unwrap().clone();
    assert_eq!(it.files, 9);
    assert!(!it.scrambled && it.children.is_empty());
    // The two unusual DACLs are recorded as exceptions.
    let special: Vec<&str> = it
        .extra
        .child_acls
        .iter()
        .map(|c| c.path.as_str())
        .collect();
    assert!(special.contains(&"closed"), "{special:?}");
    assert!(special.contains(&"Sub two\\own rules.txt"), "{special:?}");
    assert_eq!(v.state(&id), State::Locked);

    // The folder cannot be listed, entered, written to, renamed away or deleted.
    assert!(denied(std::fs::read_dir(&fx.root)));
    assert!(denied(std::fs::write(fx.root.join("new.txt"), b"x")));
    assert!(denied(std::fs::create_dir(fx.root.join("new folder"))));
    assert!(std::fs::remove_dir_all(&fx.root).is_err());
    // Children are closed even when addressed by their full path (Windows
    // lets everyone pass through folders they cannot list).
    for (rel, content) in &fx.entries {
        if rel.is_empty() {
            continue;
        }
        let p = fx.path(rel);
        if content.is_some() {
            assert!(denied(std::fs::File::open(&p)), "open {rel}");
            assert!(denied(std::fs::remove_file(&p)), "delete {rel}");
            assert!(
                denied(win::rename(&p, &sb.path().join("work\\stolen"))),
                "move out {rel}"
            );
        } else {
            assert!(denied(std::fs::read_dir(&p)), "list {rel}");
            assert!(std::fs::remove_dir(&p).is_err(), "rmdir {rel}");
        }
    }

    v.release(&id, &mut quiet()).unwrap();
    fx.assert_restored(&before);
    assert!(v.items().is_empty());
}

// --------------------------------------------------------------------------------------
// Hide
// --------------------------------------------------------------------------------------

#[test]
fn hide_file_round_trip_and_name_is_nowhere() {
    let (sb, env) = sandbox("hidefile");
    let fx = Fixture::file(
        &sb.path().join("work"),
        "Ultrasecret Passwords.kdbx",
        b"keepass bytes",
    );
    let before = fx.snapshot();
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let id = v
        .protect(&fx.root, &opts(Mode::Hide), &mut quiet())
        .unwrap();
    let it = v.item(&id).unwrap().clone();
    assert!(it.hides() && !it.scrambled);
    assert!(!win::exists(&fx.root));
    assert_eq!(v.state(&id), State::Locked);

    // A walk over everything listable in the sandbox shows no trace of the name.
    let mut names = Vec::new();
    walk(sb.path(), sb.path(), &mut names);
    for n in &names {
        let lower = n.to_lowercase();
        assert!(
            !lower.contains("ultrasecret") && !lower.contains("kdbx"),
            "{n}"
        );
    }
    assert_data_dir_is_opaque(&env, &["Ultrasecret", "kdbx", "Passwords"]);

    // The holding folder is hidden, system, not indexed, and cannot be listed.
    let hd = hold_dir(&env);
    let attrs = win::attributes(&hd).unwrap();
    assert_eq!(
        attrs & (win::ATTR_HIDDEN | win::ATTR_SYSTEM | win::ATTR_NOT_INDEXED),
        win::ATTR_HIDDEN | win::ATTR_SYSTEM | win::ATTR_NOT_INDEXED
    );
    assert!(denied(std::fs::read_dir(&hd)));
    assert!(std::fs::remove_dir_all(&hd).is_err());
    assert!(denied(win::set_attributes(&hd, win::ATTR_NORMAL)));
    assert_eq!(win::get_sddl(&hd, false).unwrap(), hold::hold_sddl());

    // With listing granted for a moment: two entries (the item and its
    // recovery copy), both random names without an extension.
    let entries = hold::list(&hd).unwrap();
    assert_eq!(entries.len(), 2);
    for e in &entries {
        let name = e.file_name().unwrap().to_string_lossy().into_owned();
        assert!(is_random_name(&name), "{name}");
    }
    assert!(denied(std::fs::read_dir(&hd)), "listing is closed again");
    let hold_path = PathBuf::from(it.hold_path.clone().unwrap());
    assert!(entries.contains(&hold_path));
    // The recovery copy is sealed.
    let rec = std::fs::read(it.extra.recovery.as_ref().unwrap()).unwrap();
    assert!(!contains(&rec, "Ultrasecret") && !contains(&rec, "kdbx"));
    // The bytes were not rewritten: same content under the random name.
    assert_eq!(std::fs::read(&hold_path).unwrap(), b"keepass bytes");

    v.release(&id, &mut quiet()).unwrap();
    fx.assert_restored(&before);
    assert!(hold::list(&hd).unwrap().is_empty());
}

#[test]
fn hide_folder_with_scrambled_names_round_trip() {
    let (sb, env) = sandbox("hidedir");
    let fx = Fixture::tree(&sb.path().join("work"), "Holiday Photos 2031");
    let before = fx.snapshot();
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let mut last = Progress::default();
    let id = v
        .protect(&fx.root, &opts(Mode::Hide), &mut |p| {
            if p.total > 0 {
                last = p
            }
        })
        .unwrap();
    assert_eq!(last.done, last.total);
    let it = v.item(&id).unwrap().clone();
    assert!(it.scrambled);
    assert_eq!(it.children.len(), fx.entries.len() - 1);
    assert_eq!(it.files, 9);
    assert!(!win::exists(&fx.root));

    // Inside the holding folder every name at every level is random; the
    // same original name in two folders got two different random names.
    let hold_path = PathBuf::from(it.hold_path.clone().unwrap());
    let mut inside = Vec::new();
    walk(&hold_path, &hold_path, &mut inside);
    assert_eq!(inside.len(), fx.entries.len() - 1);
    for rel in &inside {
        for part in rel.split('\\') {
            assert!(is_random_name(part), "{rel}");
        }
    }
    let a: Vec<&crate::model::ChildRename> = it
        .children
        .iter()
        .filter(|c| c.original == "a.txt")
        .collect();
    assert_eq!(a.len(), 2);
    assert_ne!(a[0].scrambled, a[1].scrambled);
    for c in &it.children {
        assert_ne!(c.scrambled.to_lowercase(), "0123456789abcdef");
    }
    let mut names = Vec::new();
    walk(sb.path(), sb.path(), &mut names);
    for n in &names {
        let lower = n.to_lowercase();
        for word in ["holiday", "budget", "docx", "a.txt", "deep", "sub two"] {
            assert!(!lower.contains(word), "{n} shows {word}");
        }
    }
    assert_data_dir_is_opaque(&env, &["Holiday", "Budget", "docx", "readonly hidden"]);

    v.release(&id, &mut quiet()).unwrap();
    fx.assert_restored(&before);
}

#[test]
fn hide_folder_without_scrambling_keeps_the_inside() {
    let (sb, env) = sandbox("hideplain");
    let fx = Fixture::tree(&sb.path().join("work"), "Plain");
    let before = fx.snapshot();
    let mut v = Vault::create(env, PW).unwrap();
    let mut o = opts(Mode::Hide);
    o.scramble = false;
    let id = v.protect(&fx.root, &o, &mut quiet()).unwrap();
    let it = v.item(&id).unwrap().clone();
    assert!(!it.scrambled && it.children.is_empty());
    let hold_path = PathBuf::from(it.hold_path.unwrap());
    assert!(win::exists(&hold_path.join("sub\\deep\\x.bin")));
    v.release(&id, &mut quiet()).unwrap();
    fx.assert_restored(&before);
}

#[test]
fn scramble_plan_survives_name_collisions() {
    let (sb, _env) = sandbox("collide");
    let root = sb.path().join("work\\tree");
    std::fs::create_dir_all(root.join("d")).unwrap();
    for n in ["aaaa", "bbbb", "cccc", "d\\aaaa", "d\\zzzz"] {
        std::fs::write(root.join(n), n).unwrap();
    }
    // A generator that first proposes names that already exist in the folder
    // (also in another case) and names it has handed out before.
    let mut script = vec![
        "aaaa", "BBBB", "n1", "n1", "cccc", "n2", "n2", "N1", "n3", "d", "n4", "aaaa", "n1",
        "zzzz", "n2",
    ]
    .into_iter();
    let mut counter = 0;
    let mut names = move || match script.next() {
        Some(s) => s.to_string(),
        None => {
            counter += 1;
            format!("x{counter}")
        }
    };
    let mut found = scan::Scan::default();
    scan::scan(
        &root,
        &mut scan::ScanOptions {
            acl: false,
            scramble: true,
            probe: false,
            names: &mut names,
        },
        &mut found,
    )
    .unwrap();
    assert_eq!(found.children.len(), 6);
    // No planned name equals an existing or another planned name in its folder.
    for c in &found.children {
        let siblings: Vec<&crate::model::ChildRename> =
            found.children.iter().filter(|o| o.dir == c.dir).collect();
        for o in &siblings {
            assert_ne!(c.scrambled.to_lowercase(), o.original.to_lowercase());
            if !std::ptr::eq(*o, c) {
                assert_ne!(c.scrambled.to_lowercase(), o.scrambled.to_lowercase());
            }
        }
    }
    let mut none = |_: usize, _: usize| Ok(());
    scan::scramble(&root, &found.children, &mut none).unwrap();
    let mut now = Vec::new();
    walk(&root, &root, &mut now);
    assert_eq!(now.len(), 6);
    for original in ["aaaa", "bbbb", "cccc", "d"] {
        assert!(!win::exists(&root.join(original)), "{original} still there");
    }
    // Applying again is a no-op; undoing twice as well.
    scan::scramble(&root, &found.children, &mut none).unwrap();
    scan::unscramble(&root, &found.children, &mut none).unwrap();
    scan::unscramble(&root, &found.children, &mut none).unwrap();
    for n in ["aaaa", "bbbb", "cccc", "d\\aaaa", "d\\zzzz"] {
        assert_eq!(std::fs::read(root.join(n)).unwrap(), n.as_bytes());
    }
    // Half applied, then undone.
    let mut stop_early = |done: usize, _: usize| {
        if done == 3 {
            Err(Error::Io("stop".into()))
        } else {
            Ok(())
        }
    };
    assert!(scan::scramble(&root, &found.children, &mut stop_early).is_err());
    scan::unscramble(&root, &found.children, &mut none).unwrap();
    for n in ["aaaa", "bbbb", "cccc", "d\\aaaa", "d\\zzzz"] {
        assert_eq!(std::fs::read(root.join(n)).unwrap(), n.as_bytes());
    }
}

// --------------------------------------------------------------------------------------
// Ask
// --------------------------------------------------------------------------------------

#[test]
fn ask_modes_leave_a_stand_in_and_round_trip() {
    for ask_hidden in [true, false] {
        for folder in [false, true] {
            let (sb, env) = sandbox("ask");
            let work = sb.path().join("work");
            let fx = if folder {
                Fixture::tree(&work, "Letters")
            } else {
                Fixture::file(&work, "Letter to Sam.txt", b"dear sam")
            };
            let before = fx.snapshot();
            let mut v = Vault::create(env.clone(), PW).unwrap();
            let mut o = opts(Mode::Ask);
            o.ask_hidden = ask_hidden;
            let id = v.protect(&fx.root, &o, &mut quiet()).unwrap();
            let it = v.item(&id).unwrap().clone();
            assert_eq!(it.ask_hidden, ask_hidden);
            assert_eq!(it.hides(), ask_hidden);
            assert_eq!(win::exists(&fx.root), !ask_hidden);
            if !ask_hidden {
                assert!(
                    denied(std::fs::File::open(&fx.root)) || denied(std::fs::read_dir(&fx.root))
                );
            }
            let lnk = PathBuf::from(it.standin.clone().unwrap());
            assert_eq!(lnk, crate::standin::standin_path(&fx.root));
            let s = crate::standin::read(&lnk).unwrap();
            assert_eq!(crate::standin::id_of(&s).as_deref(), Some(id.as_str()));
            assert_eq!(s.arguments, format!("--open {id}"));
            assert_eq!(
                s.target.to_string_lossy().to_lowercase(),
                env.exe.to_string_lossy().to_lowercase()
            );
            assert_eq!(s.description, "Protected item");
            assert_eq!(v.state(&id), State::Locked);

            // Opening removes the stand-in, locking again brings it back.
            let p = v.open_temporarily(&id, &mut quiet()).unwrap();
            assert_eq!(p, fx.root);
            assert!(!win::exists(&lnk));
            fx.assert_restored(&before);
            assert_eq!(v.state(&id), State::Open);
            v.relock(&id, &mut quiet()).unwrap();
            assert!(win::exists(&lnk));
            assert_eq!(v.state(&id), State::Locked);

            v.release(&id, &mut quiet()).unwrap();
            assert!(!win::exists(&lnk));
            fx.assert_restored(&before);
        }
    }
}

#[test]
fn ask_refuses_when_a_shortcut_of_that_name_exists() {
    let (sb, env) = sandbox("asklnk");
    let fx = Fixture::file(&sb.path().join("work"), "note.txt", b"n");
    std::fs::write(sb.path().join("work\\note.txt.lnk"), b"mine").unwrap();
    let mut v = Vault::create(env, PW).unwrap();
    assert!(matches!(
        v.protect(&fx.root, &opts(Mode::Ask), &mut quiet()),
        Err(Error::Refused(_))
    ));
    assert_eq!(
        std::fs::read(sb.path().join("work\\note.txt.lnk")).unwrap(),
        b"mine"
    );
    fx.assert_content();
}

// --------------------------------------------------------------------------------------
// Open temporarily / relock
// --------------------------------------------------------------------------------------

#[test]
fn open_temporarily_then_relock_picks_up_changes() {
    for mode in [Mode::Lock, Mode::Hide] {
        let (sb, env) = sandbox("openrelock");
        let mut fx = Fixture::tree(&sb.path().join("work"), "Project");
        let before = fx.snapshot();
        let mut v = Vault::create(env.clone(), PW).unwrap();
        let id = v.protect(&fx.root, &opts(mode), &mut quiet()).unwrap();
        let locked_at = v.item(&id).unwrap().locked_at;

        let p = v.open_temporarily(&id, &mut quiet()).unwrap();
        assert_eq!(p, fx.root);
        assert_eq!(v.state(&id), State::Open);
        assert!(v.item(&id).unwrap().open_since.is_some());
        fx.assert_restored(&before);
        // Opening twice changes nothing.
        v.open_temporarily(&id, &mut quiet()).unwrap();

        // The user works in it: edits, adds, renames, deletes.
        std::fs::write(fx.root.join("a.txt"), b"alpha, edited").unwrap();
        fx.entries
            .insert("a.txt".into(), Some(b"alpha, edited".to_vec()));
        std::fs::write(fx.root.join("sub\\new file.md"), b"# new").unwrap();
        fx.entries
            .insert("sub\\new file.md".into(), Some(b"# new".to_vec()));
        std::fs::rename(
            fx.root.join("Budget 2031.docx"),
            fx.root.join("Budget final.docx"),
        )
        .unwrap();
        let moved = fx.entries.remove("Budget 2031.docx").unwrap();
        fx.entries.insert("Budget final.docx".into(), moved);
        std::fs::remove_file(fx.root.join("0123456789abcdef")).unwrap();
        fx.entries.remove("0123456789abcdef");
        let edited = fx.snapshot();

        v.relock(&id, &mut quiet()).unwrap();
        assert_eq!(v.state(&id), State::Locked);
        let it = v.item(&id).unwrap().clone();
        assert!(it.open_since.is_none());
        assert_eq!(it.locked_at, locked_at);
        assert_eq!(it.files, 9);
        if mode == Mode::Hide {
            assert!(!win::exists(&fx.root));
            assert_eq!(it.children.len(), fx.entries.len() - 1);
        } else {
            assert!(denied(std::fs::read_dir(&fx.root)));
        }
        // Locking again when already locked changes nothing.
        v.relock(&id, &mut quiet()).unwrap();
        // One recovery copy, not two.
        let in_hold = hold::list(&hold_dir(&env)).unwrap().len();
        assert_eq!(in_hold, if mode == Mode::Hide { 2 } else { 1 });

        v.release(&id, &mut quiet()).unwrap();
        fx.assert_restored(&edited);
    }
}

#[test]
fn relock_all_release_all_and_groups() {
    let (sb, env) = sandbox("all");
    let work = sb.path().join("work");
    let a = Fixture::file(&work, "a.txt", b"a");
    let b = Fixture::file(&work, "b.txt", b"b");
    let c = Fixture::tree(&work, "c");
    let (sa, sb_, sc) = (a.snapshot(), b.snapshot(), c.snapshot());
    let mut v = Vault::create(env, PW).unwrap();
    let mut o = opts(Mode::Hide);
    o.group = "Work".into();
    let ia = v.protect(&a.root, &o, &mut quiet()).unwrap();
    let ib = v.protect(&b.root, &opts(Mode::Lock), &mut quiet()).unwrap();
    let ic = v.protect(&c.root, &opts(Mode::Ask), &mut quiet()).unwrap();
    assert_eq!(v.groups(), vec!["Work".to_string()]);
    v.set_group(&ib, "archive").unwrap();
    assert_eq!(v.groups(), vec!["archive".to_string(), "Work".to_string()]);
    assert_eq!(v.item(&ib).unwrap().group, "archive");
    assert!(matches!(v.set_group("nope", "x"), Err(Error::NotFound(_))));

    v.open_temporarily(&ia, &mut quiet()).unwrap();
    v.open_temporarily(&ic, &mut quiet()).unwrap();
    let relocked = v.relock_all(&mut quiet());
    assert_eq!(relocked.len(), 2);
    assert!(relocked.iter().all(|(_, r)| r.is_ok()));
    for id in [&ia, &ib, &ic] {
        assert_eq!(v.state(id), State::Locked);
    }
    assert!(
        v.health().iter().all(|h| h.problem.is_empty()),
        "{:?}",
        v.health()
    );

    v.open_temporarily(&ib, &mut quiet()).unwrap();
    let released = v.release_all(&mut quiet());
    assert_eq!(released.len(), 3);
    assert!(released.iter().all(|(_, r)| r.is_ok()), "{released:?}");
    assert!(v.items().is_empty());
    a.assert_restored(&sa);
    b.assert_restored(&sb_);
    c.assert_restored(&sc);
    assert!(matches!(
        v.release(&ia, &mut quiet()),
        Err(Error::NotFound(_))
    ));
}

// --------------------------------------------------------------------------------------
// Crash recovery
// --------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Case {
    mode: Mode,
    ask_hidden: bool,
    folder: bool,
}

/// Crashes at `step`, reopens the map and checks that the item is either
/// fully protected and in the map, or exactly as it was; then releases it and
/// checks that everything is exactly as before. Returns whether the step was hit.
fn crash_at(step: &str, case: Case) -> bool {
    let (sb, env) = sandbox("crash");
    let work = sb.path().join("work");
    let fx = if case.folder {
        Fixture::tree(&work, "Crash test folder")
    } else {
        Fixture::file(&work, "Crash test file.txt", b"must survive")
    };
    let before = fx.snapshot();
    let o = ProtectOptions {
        mode: case.mode,
        ask_hidden: case.ask_hidden,
        scramble: true,
        group: String::new(),
    };
    let op = step.split('.').next().unwrap();
    let crashing = Env {
        crash_after: Some(step.to_string()),
        ..env.clone()
    };
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let mut id = None;
    if op != "protect" {
        id = Some(v.protect(&fx.root, &o, &mut quiet()).unwrap());
    }
    if op == "relock" {
        v.open_temporarily(id.as_ref().unwrap(), &mut quiet())
            .unwrap();
    }
    drop(v);
    let mut dying = Vault::open(crashing, PW).unwrap();
    let result: Result<()> = match op {
        "protect" => dying.protect(&fx.root, &o, &mut quiet()).map(|_| ()),
        "open" => dying
            .open_temporarily(id.as_ref().unwrap(), &mut quiet())
            .map(|_| ()),
        "relock" => dying.relock(id.as_ref().unwrap(), &mut quiet()),
        "release" => dying
            .release(id.as_ref().unwrap(), &mut quiet())
            .map(|_| ()),
        other => panic!("unknown operation {other}"),
    };
    let hit = match &result {
        Ok(()) => false,
        Err(Error::Io(text)) if text.contains("simulated crash") => true,
        Err(e) => panic!("{step} {case:?}: unexpected error {e:?}"),
    };
    if hit {
        // The dead handle refuses to go on, and the journal is still there
        // without a readable name in it.
        assert!(dying.protect(&fx.root, &o, &mut quiet()).is_err());
        assert!(store::journal_exists(&env.data_dir), "{step}");
        assert_data_dir_is_opaque(&env, &["Crash test", "must survive", "Budget"]);
    }
    drop(dying);

    let mut v = Vault::open(env.clone(), PW).unwrap();
    assert!(!store::journal_exists(&env.data_dir), "{step} {case:?}");
    assert_eq!(
        v.open_notes().len(),
        hit as usize,
        "{step}: {:?}",
        v.open_notes()
    );
    assert!(v.recover().is_empty());
    match v.items().first().cloned() {
        None => {
            assert!(op == "protect" || op == "release", "{step} {case:?}");
            fx.assert_restored(&before);
            assert!(!win::exists(&crate::standin::standin_path(&fx.root)));
            assert!(
                hold::list(&hold_dir(&env)).unwrap().is_empty(),
                "{step} {case:?}"
            );
        }
        Some(it) => {
            let health = v.health();
            assert_eq!(health.len(), 1);
            assert!(health[0].problem.is_empty(), "{step} {case:?}: {health:?}");
            match health[0].state {
                State::Locked => {
                    assert!(it.open_since.is_none());
                    if it.hides() {
                        assert!(!win::exists(&fx.root), "{step} {case:?}");
                    } else {
                        assert!(acl::is_locked(&fx.root), "{step} {case:?}");
                    }
                    assert_eq!(
                        win::exists(&crate::standin::standin_path(&fx.root)),
                        case.mode == Mode::Ask
                    );
                }
                State::Open => {
                    assert!(op == "open" || op == "relock", "{step} {case:?}");
                    fx.assert_restored(&before);
                }
                other => panic!("{step} {case:?}: state {other:?}"),
            }
            // Whatever state it is in: one more round trip, then all is as before.
            if health[0].state == State::Open {
                v.relock(&it.id, &mut quiet()).unwrap();
            }
            v.release(&it.id, &mut quiet()).unwrap();
            fx.assert_restored(&before);
            assert!(
                hold::list(&hold_dir(&env)).unwrap().is_empty(),
                "{step} {case:?}"
            );
        }
    }
    hit
}

fn crash_everywhere(case: Case, expect_hits: &[&str]) {
    let mut hits = Vec::new();
    for step in JOURNAL_STEPS {
        if crash_at(step, case) {
            hits.push(*step);
        }
    }
    for e in expect_hits {
        assert!(hits.contains(e), "{case:?} never reached {e}; hit {hits:?}");
    }
    // Every operation has at least begin, recovery and save.
    for op in ["protect", "open", "relock", "release"] {
        for s in ["begin", "recovery", "save"] {
            let name = format!("{op}.{s}");
            assert!(
                hits.iter().any(|h| *h == name),
                "{case:?} never reached {name}"
            );
        }
    }
}

#[test]
fn crash_recovery_lock_folder() {
    crash_everywhere(
        Case {
            mode: Mode::Lock,
            ask_hidden: false,
            folder: true,
        },
        &[
            "protect.attributes",
            "protect.acl-children",
            "protect.acl",
            "open.acl",
            "open.acl-children",
            "open.attributes",
            "open.times",
            "relock.acl",
            "release.acl-children",
        ],
    );
}

#[test]
fn crash_recovery_lock_file() {
    crash_everywhere(
        Case {
            mode: Mode::Lock,
            ask_hidden: false,
            folder: false,
        },
        &[
            "protect.hold-folder",
            "protect.acl",
            "release.acl",
            "release.times",
        ],
    );
}

#[test]
fn crash_recovery_hide_scrambled_folder() {
    crash_everywhere(
        Case {
            mode: Mode::Hide,
            ask_hidden: false,
            folder: true,
        },
        &[
            "protect.hold-folder",
            "protect.move",
            "protect.scramble-part",
            "protect.scramble",
            "open.unscramble-part",
            "open.unscramble",
            "open.move",
            "relock.scramble-part",
            "release.unscramble-part",
            "release.move",
        ],
    );
}

#[test]
fn crash_recovery_hide_file() {
    crash_everywhere(
        Case {
            mode: Mode::Hide,
            ask_hidden: false,
            folder: false,
        },
        &["protect.move", "open.move", "relock.move", "release.move"],
    );
}

#[test]
fn crash_recovery_ask_hidden_folder() {
    crash_everywhere(
        Case {
            mode: Mode::Ask,
            ask_hidden: true,
            folder: true,
        },
        &[
            "protect.standin",
            "open.standin",
            "relock.standin",
            "release.standin",
            "protect.scramble-part",
        ],
    );
}

#[test]
fn crash_recovery_ask_locked_file() {
    crash_everywhere(
        Case {
            mode: Mode::Ask,
            ask_hidden: false,
            folder: false,
        },
        &[
            "protect.standin",
            "open.standin",
            "relock.acl",
            "release.standin",
        ],
    );
}

#[test]
fn journal_steps_are_unique_and_well_formed() {
    let mut seen = std::collections::HashSet::new();
    for s in JOURNAL_STEPS {
        assert!(seen.insert(*s), "{s} twice");
        let (op, name) = s.split_once('.').unwrap();
        assert!(["protect", "open", "relock", "release"].contains(&op));
        assert!(!name.is_empty());
    }
}

// --------------------------------------------------------------------------------------
// The map file
// --------------------------------------------------------------------------------------

#[test]
fn map_is_encrypted_and_password_rules_hold() {
    let (sb, env) = sandbox("map");
    let work = sb.path().join("work");
    assert!(!Vault::exists(&env));
    assert!(matches!(Vault::open(env.clone(), PW), Err(Error::NoVault)));
    let mut v = Vault::create(env.clone(), PW).unwrap();
    assert!(Vault::exists(&env));
    assert!(matches!(
        Vault::create(env.clone(), PW),
        Err(Error::Refused(_))
    ));
    assert!(v.verify_password(PW));
    assert!(!v.verify_password("Correct horse battery staple"));
    assert!(!v.verify_password(""));

    let a = Fixture::file(&work, "Tax return SECRETNAME.xlsx", b"numbers");
    let b = Fixture::tree(&work, "Clinic letters PRIVATEWORD");
    let (sa, sb_) = (a.snapshot(), b.snapshot());
    let mut o = opts(Mode::Hide);
    o.group = "Paperwork GROUPWORD".into();
    let ia = v.protect(&a.root, &o, &mut quiet()).unwrap();
    let ib = v.protect(&b.root, &opts(Mode::Lock), &mut quiet()).unwrap();
    let words = [
        "SECRETNAME",
        "PRIVATEWORD",
        "GROUPWORD",
        "xlsx",
        "Tax return",
        "Budget 2031",
        "\\work\\",
        "veilforge-map",
    ];
    assert_data_dir_is_opaque(&env, &words);
    for p in hold::list(&hold_dir(&env)).unwrap() {
        if p.is_file() && p.to_string_lossy() != v.item(&ia).unwrap().hold_path.clone().unwrap() {
            let bytes = std::fs::read(&p).unwrap();
            for w in words {
                assert!(!contains(&bytes, w), "recovery copy mentions {w}");
            }
        }
    }
    drop(v);

    assert!(matches!(
        Vault::open(env.clone(), "wrong password"),
        Err(Error::WrongPassword)
    ));
    let v = Vault::open(env.clone(), PW).unwrap();
    assert_eq!(v.items().len(), 2);
    assert!(!v.opened_from_backup());
    assert_eq!(v.item(&ia).unwrap().name, "Tax return SECRETNAME.xlsx");
    assert_eq!(v.item(&ia).unwrap().original, a.root.to_string_lossy());
    drop(v);

    // Change the password: the old one stops working everywhere.
    let mut v = Vault::open(env.clone(), PW).unwrap();
    assert!(matches!(
        v.change_password("not it", "new password"),
        Err(Error::WrongPassword)
    ));
    v.change_password(PW, "new password 123").unwrap();
    assert!(v.verify_password("new password 123") && !v.verify_password(PW));
    assert!(
        v.health().iter().all(|h| h.problem.is_empty()),
        "{:?}",
        v.health()
    );
    drop(v);
    assert!(matches!(
        Vault::open(env.clone(), PW),
        Err(Error::WrongPassword)
    ));
    let map = env.data_dir.join(store::MAP_FILE);
    for n in 1..=store::BACKUPS {
        let bak = store::backup_path(&map, n);
        if bak.exists() {
            let bytes = std::fs::read(&bak).unwrap();
            let h = crypto::Header::parse(crypto::MAGIC_MAP, &bytes).unwrap();
            let old = crypto::Keys::derive(PW, &h.kdf, &h.salt).unwrap();
            assert!(
                old.open(crypto::MAGIC_MAP, &bytes).is_err(),
                "backup {n} opens with the old password"
            );
        }
    }
    let mut v = Vault::open(env.clone(), "new password 123").unwrap();
    assert_eq!(v.items().len(), 2);
    v.release(&ia, &mut quiet()).unwrap();
    v.release(&ib, &mut quiet()).unwrap();
    a.assert_restored(&sa);
    b.assert_restored(&sb_);
}

#[test]
fn damaged_map_falls_back_to_a_backup() {
    for damage in ["body", "header", "truncated"] {
        let (sb, env) = sandbox("damage");
        let work = sb.path().join("work");
        let a = Fixture::file(&work, "first.txt", b"first");
        let b = Fixture::file(&work, "second.txt", b"second");
        let (sa, sb_) = (a.snapshot(), b.snapshot());
        let mut v = Vault::create(env.clone(), PW).unwrap();
        let ia = v.protect(&a.root, &opts(Mode::Hide), &mut quiet()).unwrap();
        // The backup made by the next save knows only the first item.
        let ib = v.protect(&b.root, &opts(Mode::Lock), &mut quiet()).unwrap();
        drop(v);
        let map = env.data_dir.join(store::MAP_FILE);
        let mut bytes = std::fs::read(&map).unwrap();
        match damage {
            "body" => {
                let n = bytes.len();
                bytes[n - 20] ^= 0x55;
            }
            "header" => bytes[30] ^= 0x55,
            _ => bytes.truncate(57),
        }
        std::fs::write(&map, &bytes).unwrap();

        let v = Vault::open(env.clone(), PW).unwrap();
        assert!(v.opened_from_backup(), "{damage}");
        assert!(!v.open_notes().is_empty());
        // The second item is not in the backup; its recovery copy brought it back.
        assert_eq!(v.items().len(), 2, "{damage}");
        assert_eq!(v.state(&ia), State::Locked);
        assert_eq!(v.state(&ib), State::Locked);
        drop(v);
        // The next open is an ordinary one.
        let mut v = Vault::open(env.clone(), PW).unwrap();
        assert!(!v.opened_from_backup());
        v.release(&ia, &mut quiet()).unwrap();
        v.release(&ib, &mut quiet()).unwrap();
        a.assert_restored(&sa);
        b.assert_restored(&sb_);
        drop(v);

        // Everything damaged: a clear error, and a wrong password is still told apart.
        for n in 0..=store::BACKUPS {
            let p = if n == 0 {
                map.clone()
            } else {
                store::backup_path(&map, n)
            };
            if p.exists() {
                std::fs::write(&p, b"garbage garbage garbage").unwrap();
            }
        }
        assert!(matches!(
            Vault::open(env.clone(), PW),
            Err(Error::Corrupt(_))
        ));
    }
}

#[test]
fn rebuild_from_recovery_copies_after_losing_the_data_folder() {
    let (sb, env) = sandbox("rebuild");
    let work = sb.path().join("work");
    let a = Fixture::tree(&work, "Album");
    let b = Fixture::file(&work, "locked.txt", b"locked in place");
    let c = Fixture::file(&work, "asked.pdf", b"%PDF");
    let d = Fixture::file(&work, "open one.txt", b"open");
    let snaps = [a.snapshot(), b.snapshot(), c.snapshot(), d.snapshot()];
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let ia = v.protect(&a.root, &opts(Mode::Hide), &mut quiet()).unwrap();
    let ib = v.protect(&b.root, &opts(Mode::Lock), &mut quiet()).unwrap();
    let ic = v.protect(&c.root, &opts(Mode::Ask), &mut quiet()).unwrap();
    let id = v.protect(&d.root, &opts(Mode::Hide), &mut quiet()).unwrap();
    v.open_temporarily(&id, &mut quiet()).unwrap();
    let items_before: Vec<Item> = v.items().to_vec();
    drop(v);

    // The app-data folder is lost.
    std::fs::remove_dir_all(&env.data_dir).unwrap();
    assert!(!Vault::exists(&env));
    assert!(matches!(
        rebuild_from_recovery(env.clone(), "not the password"),
        Err(Error::WrongPassword)
    ));
    assert!(
        !Vault::exists(&env),
        "a failed rebuild must not create a map"
    );

    let (mut v, n) = rebuild_from_recovery(env.clone(), PW).unwrap();
    assert_eq!(n, 4);
    assert!(v.verify_password(PW));
    for was in &items_before {
        assert_eq!(v.item(&was.id), Some(was), "{}", was.name);
    }
    assert_eq!(v.state(&ia), State::Locked);
    assert_eq!(v.state(&ib), State::Locked);
    assert_eq!(v.state(&ic), State::Locked);
    assert_eq!(v.state(&id), State::Open);
    assert!(
        v.health().iter().all(|h| h.problem.is_empty()),
        "{:?}",
        v.health()
    );
    // Nothing more to import; a second rebuild is refused.
    assert_eq!(v.import_recovery().unwrap(), 0);
    assert!(matches!(
        rebuild_from_recovery(env.clone(), PW),
        Err(Error::Refused(_))
    ));
    drop(v);
    let mut v = Vault::open(env.clone(), PW).unwrap();
    assert_eq!(v.items().len(), 4);
    for (id, r) in v.release_all(&mut quiet()) {
        assert!(r.is_ok(), "{id}: {r:?}");
    }
    a.assert_restored(&snaps[0]);
    b.assert_restored(&snaps[1]);
    c.assert_restored(&snaps[2]);
    d.assert_restored(&snaps[3]);
    assert!(hold::list(&hold_dir(&env)).unwrap().is_empty());
}

#[test]
fn rebuild_with_nothing_to_find_makes_an_empty_map() {
    let (_sb, env) = sandbox("rebuildempty");
    let (v, n) = rebuild_from_recovery(env.clone(), PW).unwrap();
    assert_eq!(n, 0);
    assert!(v.items().is_empty());
    assert!(Vault::exists(&env));
}

#[test]
fn import_recovery_adds_items_missing_from_the_map() {
    let (sb, env) = sandbox("import");
    let work = sb.path().join("work");
    let a = Fixture::file(&work, "kept.txt", b"kept");
    let b = Fixture::file(&work, "forgotten.txt", b"forgotten");
    let sb_ = b.snapshot();
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let ia = v.protect(&a.root, &opts(Mode::Hide), &mut quiet()).unwrap();
    let map = env.data_dir.join(store::MAP_FILE);
    let old_map = std::fs::read(&map).unwrap();
    let ib = v.protect(&b.root, &opts(Mode::Hide), &mut quiet()).unwrap();
    drop(v);
    // An older copy of the map comes back (restored from some backup).
    std::fs::write(&map, &old_map).unwrap();
    let mut v = Vault::open(env.clone(), PW).unwrap();
    assert_eq!(v.items().len(), 1);
    assert!(v.item(&ib).is_none());
    assert_eq!(v.import_recovery().unwrap(), 1);
    assert_eq!(v.import_recovery().unwrap(), 0);
    assert_eq!(v.state(&ib), State::Locked);
    assert_eq!(v.item(&ib).unwrap().name, "forgotten.txt");
    v.release(&ib, &mut quiet()).unwrap();
    b.assert_restored(&sb_);
    v.release(&ia, &mut quiet()).unwrap();
}

// --------------------------------------------------------------------------------------
// Refusals, warnings, special situations
// --------------------------------------------------------------------------------------

#[test]
fn refusal_rules_apply_in_the_sandbox_too() {
    let (sb, env) = sandbox("refuse");
    let work = sb.path().join("work");
    let fx = Fixture::tree(&work, "Guarded");
    let other = Fixture::file(&work, "free.txt", b"free");
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let ok = v.precheck(&fx.root);
    assert_eq!(ok.verdict, Verdict::Ok, "{:?}", ok.warnings);
    assert_eq!(
        (ok.kind, ok.files, ok.filesystem.as_str()),
        (Kind::Folder, 9, "NTFS")
    );
    assert!(ok.acl_supported && ok.scramble_default && !ok.truncated);
    assert!(ok.size > 70_000);
    let f = precheck(&env, &other.root);
    assert_eq!(
        (f.verdict.clone(), f.kind, f.size, f.files),
        (Verdict::Ok, Kind::File, 4, 1)
    );
    assert!(!f.scramble_default);

    let id = v
        .protect(&fx.root, &opts(Mode::Lock), &mut quiet())
        .unwrap();
    let refused = |v: &Vault, p: &Path| {
        let pre = v.precheck(p);
        assert_eq!(pre.verdict, Verdict::Refuse, "{}", p.display());
        pre.refusal.unwrap()
    };
    assert!(refused(&v, &fx.root).contains("already protected"));
    assert!(refused(&v, &fx.root.join("a.txt")).contains("inside"));
    assert!(refused(&v, &work).contains("contains"));
    refused(&v, &env.data_dir);
    refused(&v, &env.data_dir.join(store::MAP_FILE));
    refused(&v, &hold_dir(&env));
    refused(&v, sb.path());
    refused(&v, &work.join("does not exist.txt"));
    refused(&v, &std::env::temp_dir());
    refused(&v, Path::new("C:\\"));
    if let Some(w) = std::env::var_os("SystemRoot") {
        refused(&v, Path::new(&w));
        refused(&v, &Path::new(&w).join("System32"));
    }
    if let Some(p) = std::env::var_os("USERPROFILE") {
        refused(&v, Path::new(&p));
    }
    for p in [&fx.root, &work, &env.data_dir, &hold_dir(&env)] {
        assert!(matches!(
            v.protect(p, &opts(Mode::Hide), &mut quiet()),
            Err(Error::Refused(_))
        ));
    }
    assert!(matches!(
        v.protect(
            &work.join("does not exist.txt"),
            &opts(Mode::Hide),
            &mut quiet()
        ),
        Err(Error::NotFound(_))
    ));
    assert_eq!(v.items().len(), 1);
    assert!(!store::journal_exists(&env.data_dir));
    // The item's own stand-in and holding entry cannot be protected either.
    let ask = v
        .protect(&other.root, &opts(Mode::Ask), &mut quiet())
        .unwrap();
    let it = v.item(&ask).unwrap().clone();
    refused(&v, Path::new(it.standin.as_ref().unwrap()));
    refused(&v, Path::new(it.hold_path.as_ref().unwrap()));
    v.release(&ask, &mut quiet()).unwrap();
    v.release(&id, &mut quiet()).unwrap();
    assert_eq!(v.precheck(&fx.root).verdict, Verdict::Ok);
}

#[test]
fn links_are_refused_as_items_and_never_followed_inside() {
    let (sb, env) = sandbox("links");
    let work = sb.path().join("work");
    let outside = Fixture::tree(&work, "Outside");
    let outside_before = outside.snapshot();
    let tree = Fixture::tree(&work, "With link");
    let link = tree.root.join("junction to outside");
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&outside.root)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    assert!(win::stat(&link).unwrap().is_link());
    let top_link = work.join("top junction");
    std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&top_link)
        .arg(&outside.root)
        .output()
        .unwrap();

    let mut v = Vault::create(env.clone(), PW).unwrap();
    let pre = v.precheck(&top_link);
    assert_eq!(pre.verdict, Verdict::Refuse);
    assert!(pre.refusal.unwrap().contains("link"));
    assert!(matches!(
        v.protect(&top_link, &opts(Mode::Lock), &mut quiet()),
        Err(Error::Refused(_))
    ));
    // The scan counts the link as one entry and does not look behind it.
    assert_eq!(v.precheck(&tree.root).files, 10);

    for mode in [Mode::Lock, Mode::Hide] {
        let id = v.protect(&tree.root, &opts(mode), &mut quiet()).unwrap();
        assert_eq!(v.item(&id).unwrap().extra.skipped_links, 1);
        // Nothing behind the link was locked or renamed.
        outside.assert_restored(&outside_before);
        v.release(&id, &mut quiet()).unwrap();
        assert!(win::stat(&link).unwrap().is_link());
        // Compared before this test reads through the link itself (which
        // would move the target folder's last-access time).
        outside.assert_restored(&outside_before);
        assert_eq!(
            std::fs::read(link.join("a.txt")).unwrap(),
            b"alpha",
            "the link still works"
        );
        outside.assert_restored(&outside.snapshot_after_own_read(&outside_before));
    }
    std::fs::remove_dir(&link).unwrap();
    std::fs::remove_dir(&top_link).unwrap();
    tree.assert_content();
}

#[test]
fn file_in_use_is_reported_and_nothing_changes() {
    let (sb, env) = sandbox("inuse");
    let work = sb.path().join("work");
    let file = Fixture::file(&work, "open in editor.txt", b"busy");
    let tree = Fixture::tree(&work, "Busy folder");
    let mut v = Vault::create(env.clone(), PW).unwrap();

    // Another program holds the file without sharing it.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&file.root)
        .unwrap();
    let sf = file.snapshot();
    for mode in [Mode::Lock, Mode::Hide, Mode::Ask] {
        let r = v.protect(&file.root, &opts(mode), &mut quiet());
        assert!(matches!(r, Err(Error::InUse(_))), "{mode:?}: {r:?}");
    }
    file.assert_restored(&sf);
    drop(held);
    // A file inside a folder is held.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(tree.root.join("sub\\deep\\x.bin"))
        .unwrap();
    let st = tree.snapshot();
    for mode in [Mode::Lock, Mode::Hide, Mode::Ask] {
        let r = v.protect(&tree.root, &opts(mode), &mut quiet());
        println!("MEASURED protect folder with a held file, {mode:?}: {r:?}");
        assert!(matches!(r, Err(Error::InUse(_))), "{mode:?}: {r:?}");
        // Exactly as before, names, dates and permissions included.
        let after = tree.snapshot();
        assert_eq!(after, st, "{mode:?}");
    }
    drop(held);
    assert!(v.items().is_empty());
    assert!(!store::journal_exists(&env.data_dir));
    file.assert_content();
    tree.assert_content();
    assert!(!win::exists(&crate::standin::standin_path(&file.root)));
    assert!(hold::list(&hold_dir(&env)).unwrap().is_empty());
    // Once closed, it works.
    let sf = file.snapshot();
    let id = v
        .protect(&file.root, &opts(Mode::Hide), &mut quiet())
        .unwrap();
    v.release(&id, &mut quiet()).unwrap();
    file.assert_restored(&sf);
}

#[test]
fn long_paths_work() {
    let (sb, env) = sandbox("long");
    let mut dir = sb.path().join("work");
    while dir.as_os_str().len() < 330 {
        dir = dir.join("a folder with a fairly long name 0123456789");
    }
    std::fs::create_dir_all(win::long(&dir)).unwrap();
    let file = Fixture::file(
        &dir,
        "a file far down the tree with a long name.txt",
        b"deep",
    );
    let tree = Fixture::tree(&dir, "tree far down");
    assert!(file.root.as_os_str().len() > 300);
    let (sf, st) = (file.snapshot(), tree.snapshot());
    let mut v = Vault::create(env.clone(), PW).unwrap();
    assert_eq!(v.precheck(&file.root).verdict, Verdict::Ok);
    for mode in [Mode::Hide, Mode::Lock, Mode::Ask] {
        let i1 = v.protect(&file.root, &opts(mode), &mut quiet()).unwrap();
        let i2 = v.protect(&tree.root, &opts(mode), &mut quiet()).unwrap();
        assert_eq!(v.state(&i1), State::Locked, "{mode:?}");
        assert_eq!(v.state(&i2), State::Locked, "{mode:?}");
        assert!(
            v.health().iter().all(|h| h.problem.is_empty()),
            "{:?}",
            v.health()
        );
        assert!(!v.item(&i1).unwrap().original.starts_with("\\\\?\\"));
        if mode != Mode::Lock {
            assert!(!win::exists(&file.root) && !win::exists(&tree.root));
        } else {
            assert!(denied(std::fs::File::open(win::long(&file.root))));
        }
        v.open_temporarily(&i2, &mut quiet()).unwrap();
        v.relock(&i2, &mut quiet()).unwrap();
        v.release(&i1, &mut quiet()).unwrap();
        v.release(&i2, &mut quiet()).unwrap();
        file.assert_restored(&sf);
        tree.assert_restored(&st);
    }
}

#[test]
fn volume_without_acls_is_hide_only_with_reduced_protection() {
    let (sb, env) = sandbox("fat");
    volume::set_override(
        sb.path(),
        Some(volume::FsInfo {
            name: "exFAT".into(),
            acl: false,
            remote: false,
        }),
    );
    struct Reset(PathBuf);
    impl Drop for Reset {
        fn drop(&mut self) {
            volume::set_override(&self.0, None);
        }
    }
    let _reset = Reset(sb.path().to_path_buf());
    let work = sb.path().join("work");
    let fx = Fixture::tree(&work, "On a stick");
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let pre = v.precheck(&fx.root);
    assert_eq!(pre.verdict, Verdict::Warn);
    assert!(!pre.acl_supported);
    assert_eq!(pre.filesystem, "exFAT");
    assert!(pre
        .warnings
        .iter()
        .any(|w| w.contains("exFAT") && w.contains("reduced")));

    assert!(matches!(
        v.protect(&fx.root, &opts(Mode::Lock), &mut quiet()),
        Err(Error::Unsupported(_))
    ));
    let mut ask_locked = opts(Mode::Ask);
    ask_locked.ask_hidden = false;
    assert!(matches!(
        v.protect(&fx.root, &ask_locked, &mut quiet()),
        Err(Error::Unsupported(_))
    ));
    fx.assert_content();

    let id = v
        .protect(&fx.root, &opts(Mode::Hide), &mut quiet())
        .unwrap();
    let it = v.item(&id).unwrap().clone();
    assert!(it.reduced && it.saved.sddl.is_empty());
    assert!(!win::exists(&fx.root));
    // The holding folder is hidden + system but, without ACLs, still listable.
    let hd = hold_dir(&env);
    let attrs = win::attributes(&hd).unwrap();
    assert_eq!(
        attrs & (win::ATTR_HIDDEN | win::ATTR_SYSTEM),
        win::ATTR_HIDDEN | win::ATTR_SYSTEM
    );
    assert_ne!(win::get_sddl(&hd, false).unwrap(), hold::hold_sddl());
    assert!(std::fs::read_dir(&hd).is_ok());
    assert_eq!(v.state(&id), State::Locked);
    assert!(v.health()[0].problem.is_empty());
    v.open_temporarily(&id, &mut quiet()).unwrap();
    fx.assert_content();
    v.relock(&id, &mut quiet()).unwrap();
    v.release(&id, &mut quiet()).unwrap();
    fx.assert_content();
    assert_eq!(v.remove_empty_holds(), 1);
}

#[test]
fn warnings_for_network_cloud_and_large_trees() {
    let (sb, env) = sandbox("warn");
    let work = sb.path().join("work");
    let fx = Fixture::tree(&work, "Synced");
    let mut sp = safety::Special::real(&env);
    assert_eq!(
        safety::precheck_with(&env, &sp, &fx.root, &[], SCAN_LIMIT).verdict,
        Verdict::Ok
    );
    // The folder is declared a cloud folder.
    sp.cloud.push(work.clone());
    let pre = safety::precheck_with(&env, &sp, &fx.root, &[], SCAN_LIMIT);
    assert_eq!(pre.verdict, Verdict::Warn);
    assert!(pre
        .warnings
        .iter()
        .any(|w| w.contains("cloud") && w.contains("delete")));
    sp.cloud.clear();
    // The scan stops at its limit and says so.
    let pre = safety::precheck_with(&env, &sp, &fx.root, &[], 5);
    assert!(pre.truncated && !pre.scramble_default);
    assert_eq!(pre.verdict, Verdict::Warn);
    assert!(pre.warnings.iter().any(|w| w.contains("very large")));
    // A network drive.
    volume::set_override(
        sb.path(),
        Some(volume::FsInfo {
            name: "NTFS".into(),
            acl: true,
            remote: true,
        }),
    );
    let pre = safety::precheck_with(&env, &sp, &fx.root, &[], SCAN_LIMIT);
    volume::set_override(sb.path(), None);
    assert_eq!(pre.verdict, Verdict::Warn);
    assert!(pre.warnings.iter().any(|w| w.contains("network")));
}

#[test]
fn holding_folder_on_another_volume_is_refused() {
    let (sb, env) = sandbox("xvol");
    // Find a second local volume, if this PC has one; otherwise there is
    // nothing to test here (the rule itself is covered by `dir_for`).
    let here = volume::root_of(sb.path()).unwrap();
    let Some(other) = win::local_drive_roots()
        .into_iter()
        .find(|r| !r.eq_ignore_ascii_case(&here))
    else {
        return;
    };
    // Only the decision is tested: nothing is created on the other volume.
    let pretend_item = PathBuf::from(&other).join("some folder\\file.txt");
    let existing_hold = env.hold_root.clone().unwrap();
    std::fs::create_dir_all(&existing_hold).unwrap();
    assert!(matches!(
        hold::dir_for(&env, &pretend_item),
        Err(Error::CrossVolume(_))
    ));
    assert!(hold::dir_for(&env, &sb.path().join("work\\x")).is_ok());
}

// --------------------------------------------------------------------------------------
// Health, repair, concurrency
// --------------------------------------------------------------------------------------

#[test]
fn tampering_is_noticed_and_repaired() {
    let (sb, env) = sandbox("health");
    let work = sb.path().join("work");
    let locked = Fixture::tree(&work, "Locked folder");
    let hidden = Fixture::file(&work, "Hidden.txt", b"hidden");
    let asked = Fixture::file(&work, "Asked.txt", b"asked");
    let snaps = [locked.snapshot(), hidden.snapshot(), asked.snapshot()];
    let mut v = Vault::create(env.clone(), PW).unwrap();
    let il = v
        .protect(&locked.root, &opts(Mode::Lock), &mut quiet())
        .unwrap();
    let ih = v
        .protect(&hidden.root, &opts(Mode::Hide), &mut quiet())
        .unwrap();
    let ia = v
        .protect(&asked.root, &opts(Mode::Ask), &mut quiet())
        .unwrap();
    assert!(v
        .health()
        .iter()
        .all(|h| h.state == State::Locked && h.problem.is_empty() && !h.repairable));
    assert_eq!(v.repair(&il).unwrap(), "Nothing needed repairing.");

    // Someone resets the permissions of the locked folder in the Security tab.
    let original_dacl = acl::dacl_part(&v.item(&il).unwrap().saved.sddl).to_string();
    win::set_dacl(&locked.root, &original_dacl).unwrap();
    assert!(std::fs::read_dir(&locked.root).is_ok());
    assert_eq!(v.state(&il), State::Tampered);
    let h = v.health().into_iter().find(|h| h.id == il).unwrap();
    assert!(h.repairable && !h.problem.is_empty());
    v.repair(&il).unwrap();
    assert_eq!(v.state(&il), State::Locked);
    assert!(denied(std::fs::read_dir(&locked.root)));

    // The stand-in is deleted.
    let lnk = PathBuf::from(v.item(&ia).unwrap().standin.clone().unwrap());
    std::fs::remove_file(&lnk).unwrap();
    assert_eq!(v.state(&ia), State::Tampered);
    v.repair(&ia).unwrap();
    assert!(win::exists(&lnk));
    assert_eq!(v.state(&ia), State::Locked);

    // The holding folder is opened up and unhidden.
    let hd = hold_dir(&env);
    win::set_dacl(&hd, &acl::open_sddl()).unwrap();
    win::set_attributes(&hd, win::ATTR_NORMAL).unwrap();
    assert_eq!(v.state(&ih), State::Tampered);
    assert_eq!(
        v.state(&il),
        State::Locked,
        "a locked item does not depend on it"
    );
    v.repair(&ih).unwrap();
    assert_eq!(v.state(&ih), State::Locked);
    assert!(denied(std::fs::read_dir(&hd)));

    // A recovery copy is lost: still locked, but reported and repairable.
    let rec = PathBuf::from(v.item(&ih).unwrap().extra.recovery.clone().unwrap());
    std::fs::remove_file(&rec).unwrap();
    assert_eq!(v.state(&ih), State::Locked);
    let h = v.health().into_iter().find(|h| h.id == ih).unwrap();
    assert!(h.repairable && h.problem.contains("recovery"));
    v.repair(&ih).unwrap();
    assert!(win::exists(&rec));
    assert!(
        v.health().iter().all(|h| h.problem.is_empty()),
        "{:?}",
        v.health()
    );

    // The hidden item is taken out of the holding folder: missing, not repairable.
    let hp = PathBuf::from(v.item(&ih).unwrap().hold_path.clone().unwrap());
    let elsewhere = work.join("taken away");
    win::rename(&hp, &elsewhere).unwrap();
    assert_eq!(v.state(&ih), State::Missing);
    let h = v.health().into_iter().find(|h| h.id == ih).unwrap();
    assert!(!h.repairable);
    assert!(matches!(v.repair(&ih), Err(Error::NotFound(_))));
    assert!(matches!(
        v.release(&ih, &mut quiet()),
        Err(Error::NotFound(_))
    ));
    assert!(
        v.item(&ih).is_some(),
        "a failed release keeps the item in the map"
    );
    win::rename(&elsewhere, &hp).unwrap();
    assert_eq!(v.state(&ih), State::Locked);

    // Something else takes the original name while the item is hidden.
    std::fs::write(&hidden.root, b"squatter").unwrap();
    assert!(matches!(
        v.release(&ih, &mut quiet()),
        Err(Error::Refused(_))
    ));
    assert_eq!(std::fs::read(&hidden.root).unwrap(), b"squatter");
    assert_eq!(v.state(&ih), State::Locked);
    std::fs::remove_file(&hidden.root).unwrap();

    for id in [&il, &ih, &ia] {
        v.release(id, &mut quiet()).unwrap();
    }
    locked.assert_restored(&snaps[0]);
    hidden.assert_restored(&snaps[1]);
    asked.assert_restored(&snaps[2]);
    assert_eq!(v.state("no such id"), State::Missing);
}

#[test]
fn two_handles_on_one_data_folder_stay_consistent() {
    let (sb, env) = sandbox("two");
    let work = sb.path().join("work");
    let a = Fixture::file(&work, "a.txt", b"a");
    let b = Fixture::file(&work, "b.txt", b"b");
    let c = Fixture::file(&work, "c.txt", b"c");
    let mut one = Vault::create(env.clone(), PW).unwrap();
    let mut two = Vault::open(env.clone(), PW).unwrap();
    assert!(!two.refresh().unwrap());

    let ia = one
        .protect(&a.root, &opts(Mode::Hide), &mut quiet())
        .unwrap();
    assert!(two.items().is_empty(), "not refreshed yet");
    assert!(two.refresh().unwrap());
    assert!(!two.refresh().unwrap());
    assert_eq!(two.items().len(), 1);

    // A mutating call reloads by itself: nothing of the other handle is lost.
    let ib = two
        .protect(&b.root, &opts(Mode::Lock), &mut quiet())
        .unwrap();
    assert_eq!(two.items().len(), 2);
    let ic = one
        .protect(&c.root, &opts(Mode::Ask), &mut quiet())
        .unwrap();
    assert_eq!(one.items().len(), 3);
    // The second handle cannot protect what the first already protected.
    assert!(matches!(
        two.protect(&c.root, &opts(Mode::Lock), &mut quiet()),
        Err(Error::Refused(_)) | Err(Error::NotFound(_))
    ));
    two.open_temporarily(&ia, &mut quiet()).unwrap();
    assert!(one.refresh().unwrap());
    assert_eq!(one.state(&ia), State::Open);
    one.relock(&ia, &mut quiet()).unwrap();
    two.release(&ia, &mut quiet()).unwrap();
    assert!(matches!(
        one.release(&ia, &mut quiet()),
        Err(Error::NotFound(_))
    ));
    assert_eq!(one.items().len(), 2);

    // Both at once from two threads.
    let (id_b, id_c) = (ib.clone(), ic.clone());
    let t1 = std::thread::spawn(move || {
        for _ in 0..3 {
            one.open_temporarily(&id_b, &mut |_| {}).unwrap();
            one.relock(&id_b, &mut |_| {}).unwrap();
        }
        one
    });
    let t2 = std::thread::spawn(move || {
        for _ in 0..3 {
            two.open_temporarily(&id_c, &mut |_| {}).unwrap();
            two.relock(&id_c, &mut |_| {}).unwrap();
        }
        two
    });
    let mut one = t1.join().unwrap();
    let mut two = t2.join().unwrap();
    one.refresh().unwrap();
    two.refresh().unwrap();
    assert_eq!(one.items(), two.items());
    assert!(one
        .health()
        .iter()
        .all(|h| h.state == State::Locked && h.problem.is_empty()));

    // A password change in one handle locks the other one out.
    one.change_password(PW, "another password").unwrap();
    assert!(matches!(two.refresh(), Err(Error::WrongPassword)));
    assert!(matches!(
        two.release(&ib, &mut quiet()),
        Err(Error::WrongPassword)
    ));
    one.release(&ib, &mut quiet()).unwrap();
    one.release(&ic, &mut quiet()).unwrap();
    a.assert_content();
    b.assert_content();
    c.assert_content();
}

#[test]
fn vault_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Vault>();
}

#[test]
fn cleanup_refuses_anything_but_a_sandbox() {
    assert!(!cleanup::force_remove(&std::env::temp_dir()));
    assert!(!cleanup::force_remove(Path::new(
        "C:\\Users\\alex\\Documents"
    )));
    let (sb, _env) = sandbox("cleanup");
    assert!(!cleanup::force_remove(&sb.path().join("work")));
    let path = sb.path().to_path_buf();
    // A locked folder with a hidden read-only file inside goes away all the same.
    let inner = path.join("work\\locked");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("f.txt"), b"x").unwrap();
    win::set_attributes(&inner.join("f.txt"), win::ATTR_READONLY | win::ATTR_HIDDEN).unwrap();
    win::set_dacl(&inner.join("f.txt"), acl::lock_sddl()).unwrap();
    win::set_dacl(&inner, acl::lock_sddl()).unwrap();
    assert!(std::fs::remove_dir_all(&path).is_err());
    drop(sb);
    assert!(!win::exists(&path));
}
