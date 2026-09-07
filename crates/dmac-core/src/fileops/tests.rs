//! End-to-end tests for the engine.
//!
//! Every one of these builds its own [`TempDir`] and works only inside it. That
//! is not a style rule: this module drives code that deletes things, and a test
//! that can reach outside its sandbox is one bad path join away from deleting
//! the developer's home directory. The two operations that could escape — the
//! recursive delete and the overwrite path — are pointed *at* a symlink leading
//! out of the tree in [`deleting_a_tree_never_follows_a_link_out_of_it`] and
//! the target is asserted to survive.
//!
//! The platform trash is never exercised for real either: a test that put files
//! in the developer's own bin would be leaving rubbish outside its TempDir, so
//! [`TrashCan`] is injected instead.

use super::*;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::TempDir;

// ---- driving a job -------------------------------------------------------

/// Everything a driven job said.
#[derive(Default)]
struct Ran {
    finished: Option<Outcome>,
    conflicts: Vec<Conflict>,
    warnings: Vec<Warning>,
    failures: Vec<Failure>,
    progress: Vec<Progress>,
}

impl Ran {
    fn outcome(&self) -> &Outcome {
        self.finished
            .as_ref()
            .expect("every job ends with an outcome")
    }
    fn status(&self) -> &Status {
        &self.outcome().status
    }
    fn warned(&self, kind: WarningKind) -> bool {
        self.warnings.iter().any(|w| w.kind == kind)
    }
    fn last_progress(&self) -> &Progress {
        self.progress.last().expect("a job reports progress")
    }
}

/// Answer nothing, because nothing should be asked.
fn no_conflicts(c: &Conflict, _n: usize) -> Option<Resolution> {
    panic!("unexpected conflict on {}", c.destination.path.display());
}

/// Run a job to completion on this thread, answering conflicts with `answer`.
///
/// `answer` returning `None` drops the prompt, which is how "the dialog closed
/// without an answer" is reproduced.
fn drive(
    job: Job,
    opts: Options,
    mut answer: impl FnMut(&Conflict, usize) -> Option<Resolution>,
) -> Ran {
    let (_handle, mut rx) = spawn(job, opts).expect("a job thread starts");
    let mut ran = Ran::default();
    let mut n = 0;
    while let Some(event) = rx.blocking_recv() {
        match event {
            JobEvent::Started(_) => {}
            JobEvent::Progress(p) => ran.progress.push(p),
            JobEvent::Warning(w) => ran.warnings.push(w),
            JobEvent::Failure(f) => ran.failures.push(f),
            JobEvent::Conflict(prompt) => {
                ran.conflicts.push(prompt.conflict().clone());
                let reply = answer(prompt.conflict(), n);
                n += 1;
                match reply {
                    Some(r) => prompt.answer(r),
                    None => drop(prompt),
                }
            }
            JobEvent::Finished(o) => ran.finished = Some(o),
        }
    }
    ran
}

/// Run a job and cancel it the moment `trigger` says so.
fn drive_cancelling(job: Job, opts: Options, trigger: impl Fn(&Progress) -> bool) -> Ran {
    let (handle, mut rx) = spawn(job, opts).expect("a job thread starts");
    let mut ran = Ran::default();
    while let Some(event) = rx.blocking_recv() {
        match event {
            JobEvent::Progress(p) => {
                if trigger(&p) {
                    handle.cancel();
                }
                ran.progress.push(p);
            }
            JobEvent::Warning(w) => ran.warnings.push(w),
            JobEvent::Failure(f) => ran.failures.push(f),
            JobEvent::Conflict(prompt) => drop(prompt),
            JobEvent::Finished(o) => ran.finished = Some(o),
            JobEvent::Started(_) => {}
        }
    }
    ran
}

// ---- building trees ------------------------------------------------------

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every `.dmac-part` left anywhere under `root`.
fn leftover_parts(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if is_partial(&path) {
                out.push(path.clone());
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out
}

fn copy(sources: &[&Path], destination: &Path) -> Job {
    Job::Copy {
        sources: sources.iter().map(|p| p.to_path_buf()).collect(),
        destination: destination.to_path_buf(),
    }
}

fn move_job(sources: &[&Path], destination: &Path) -> Job {
    Job::Move {
        sources: sources.iter().map(|p| p.to_path_buf()).collect(),
        destination: destination.to_path_buf(),
    }
}

/// Force the copy-verify-delete path that a move across two disks takes. A
/// machine with one filesystem would otherwise never execute the code that can
/// actually lose a file.
fn cross_device() -> Options {
    Options {
        always_copy_on_move: true,
        // A real cross-device move cannot reflink — the two filesystems share
        // no extents — so neither does the simulation. Leaving it on would let
        // these tests take an instant clone and skip the streaming, hashing and
        // re-reading that the path they are about actually does.
        reflink: false,
        ..Options::default()
    }
}

// ---- copy ----------------------------------------------------------------

#[test]
fn a_copied_file_is_byte_identical_and_keeps_its_metadata() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/report.bin");
    let dst_dir = t.path().join("dst");
    let payload: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    write(&src, &payload);
    fs::create_dir_all(&dst_dir).unwrap();

    let past = filetime::FileTime::from_unix_time(1_400_000_000, 0);
    filetime::set_file_times(&src, past, past).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&src, fs::Permissions::from_mode(0o751)).unwrap();
    }

    let ran = drive(copy(&[&src], &dst_dir), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);

    let out = dst_dir.join("report.bin");
    assert_eq!(read(&out), payload);
    let meta = out.symlink_metadata().unwrap();
    assert_eq!(
        filetime::FileTime::from_last_modification_time(&meta),
        past,
        "an mtime that moves makes every backup look rewritten"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(meta.permissions().mode() & 0o777, 0o751);
    }
    assert!(leftover_parts(t.path()).is_empty());
}

#[test]
fn a_tree_is_copied_with_its_shape_intact() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/tree");
    write(&src.join("top.txt"), b"top");
    write(&src.join("a/one.txt"), b"one");
    write(&src.join("a/b/two.txt"), b"two");
    fs::create_dir_all(src.join("empty")).unwrap();
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);

    assert_eq!(read(&dst.join("tree/top.txt")), b"top");
    assert_eq!(read(&dst.join("tree/a/one.txt")), b"one");
    assert_eq!(read(&dst.join("tree/a/b/two.txt")), b"two");
    assert!(
        dst.join("tree/empty").is_dir(),
        "an empty directory is part of the shape"
    );
    assert_eq!(ran.outcome().files_done, 3);
}

#[test]
fn progress_totals_match_what_actually_moved() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    for i in 0..12 {
        write(&src.join(format!("f{i}.bin")), &vec![b'x'; 1000]);
    }
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    let last = ran.last_progress();
    assert_eq!(last.files_total, Some(12));
    assert_eq!(last.bytes_total, Some(12_000));
    assert_eq!(last.files_done, 12);
    assert_eq!(last.bytes_done, 12_000);
    assert_eq!(last.percent(), Some(100.0));

    let scanning = ran
        .progress
        .iter()
        .find(|p| p.phase == Phase::Scanning)
        .expect("the scan reports too");
    assert_eq!(
        scanning.percent(),
        None,
        "there is no denominator during a scan, so there is no percentage"
    );
}

/// The rule from the agent brief, end to end: coalescing happens at the source.
#[test]
fn two_thousand_files_do_not_produce_two_thousand_messages() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    for i in 0..2000 {
        write(&src.join(format!("f{i:05}")), b"x");
    }
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.outcome().files_done, 2000);
    assert!(
        ran.progress.len() < 100,
        "{} progress messages for 2000 files",
        ran.progress.len()
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_is_copied_as_a_link_and_never_followed() {
    let t = TempDir::new().unwrap();
    let outside = t.path().join("outside");
    write(&outside.join("secret.txt"), b"not yours to copy");
    let src = t.path().join("src/tree");
    write(&src.join("real.txt"), b"real");
    std::os::unix::fs::symlink(&outside, src.join("link")).unwrap();
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);

    let link = dst.join("tree/link");
    assert!(
        link.symlink_metadata().unwrap().is_symlink(),
        "the link arrived as a link"
    );
    assert_eq!(
        fs::read_link(&link).unwrap(),
        outside,
        "and points where it always did"
    );
    // Following it would have copied the target's *contents* into the tree.
    // Counted without following, the destination holds one real file.
    let real_files: Vec<PathBuf> = fs::read_dir(dst.join("tree"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().symlink_metadata().is_ok_and(|m| m.is_file()))
        .map(|e| e.path())
        .collect();
    assert_eq!(real_files, vec![dst.join("tree/real.txt")]);
}

/// Overwriting means the destination is occupied, which is exactly the case
/// where creating a link straight at the destination would fail.
#[cfg(unix)]
#[test]
fn a_link_can_replace_a_file_that_is_already_there() {
    let t = TempDir::new().unwrap();
    let target = t.path().join("target.txt");
    write(&target, b"target");
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    std::os::unix::fs::symlink(&target, src.join("thing")).unwrap();
    let dst = t.path().join("dst");
    write(&dst.join("thing"), b"an ordinary file, in the way");

    let ran = drive(
        copy(&[&src.join("thing")], &dst),
        Options::default(),
        |_, _| Some(Resolution::once(ConflictChoice::Overwrite)),
    );
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    let out = dst.join("thing");
    assert!(out.symlink_metadata().unwrap().is_symlink());
    assert_eq!(fs::read_link(&out).unwrap(), target);
    assert!(leftover_parts(t.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn a_symlink_loop_terminates_instead_of_walking_for_ever() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/tree");
    write(&src.join("real.txt"), b"real");
    std::os::unix::fs::symlink(&src, src.join("loop")).unwrap();
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    // The default: the link is copied as a link, so there is no loop to walk.
    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed);
    assert!(
        dst.join("tree/loop")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );

    // Following: the cycle has to be *detected*, because here it is real.
    let dst2 = t.path().join("dst2");
    fs::create_dir_all(&dst2).unwrap();
    let opts = Options {
        follow_symlinks: true,
        ..Options::default()
    };
    let ran = drive(copy(&[&src], &dst2), opts, no_conflicts);
    assert!(ran.warned(WarningKind::SourceKept), "{:?}", ran.warnings);
    assert_eq!(read(&dst2.join("tree/real.txt")), b"real");
}

#[test]
fn a_very_deep_tree_does_not_blow_the_stack() {
    let t = TempDir::new().unwrap();
    let mut deep = t.path().join("src");
    for _ in 0..300 {
        deep = deep.join("d");
    }
    write(&deep.join("bottom.txt"), b"bottom");
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(
        copy(&[&t.path().join("src")], &dst),
        Options::default(),
        no_conflicts,
    );
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    let mut out = dst.join("src");
    for _ in 0..300 {
        out = out.join("d");
    }
    assert_eq!(read(&out.join("bottom.txt")), b"bottom");
}

#[cfg(unix)]
#[test]
fn a_fifo_is_skipped_rather_than_opened_and_waited_on_for_ever() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    write(&src.join("ordinary.txt"), b"fine");
    let fifo = src.join("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !made {
        return; // no mkfifo here; nothing to prove
    }
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(read(&dst.join("src/ordinary.txt")), b"fine");
    assert!(!dst.join("src/pipe").exists(), "a fifo is not copied");
    assert_eq!(ran.outcome().skipped, 1);
    assert!(
        ran.warnings.iter().any(|w| w.detail.contains("fifo")),
        "{:?}",
        ran.warnings
    );
}

#[cfg(unix)]
#[test]
fn one_unreadable_directory_does_not_cost_the_rest_of_the_job() {
    use std::os::unix::fs::PermissionsExt;
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    write(&src.join("readable/a.txt"), b"a");
    write(&src.join("readable/b.txt"), b"b");
    let locked = src.join("locked");
    write(&locked.join("hidden.txt"), b"hidden");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        // Running as root: the permission bits are not enforced, and the test
        // would prove nothing.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), |_, _| {
        Some(Resolution::once(ConflictChoice::Skip))
    });

    assert_eq!(read(&dst.join("src/readable/a.txt")), b"a");
    assert_eq!(read(&dst.join("src/readable/b.txt")), b"b");
    assert_eq!(ran.status(), &Status::CompletedWithFailures);
    assert!(
        ran.failures
            .iter()
            .any(|f| f.kind == FailureKind::PermissionDenied),
        "{:?}",
        ran.failures
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn hardlinked_files_arrive_hardlinked() {
    use std::os::unix::fs::MetadataExt;
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    let a = src.join("a.bin");
    write(&a, b"shared payload");
    fs::hard_link(&a, src.join("b.bin")).unwrap();
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);

    let x = dst.join("src/a.bin").symlink_metadata().unwrap();
    let y = dst.join("src/b.bin").symlink_metadata().unwrap();
    assert_eq!(
        (x.dev(), x.ino()),
        (y.dev(), y.ino()),
        "two names for one file must not become two files"
    );
    assert_eq!(
        ran.last_progress().bytes_total,
        Some("shared payload".len() as u64),
        "the shared bytes are promised once, so the bar can reach the end"
    );
}

/// Either a crashed run or another job is writing it. The two are
/// indistinguishable, so neither is overwritten.
#[test]
fn a_partial_file_already_in_the_way_is_never_stolen() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/data.bin");
    write(&src, b"the real thing");
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();
    let part = dst.join("data.bin.dmac-part");
    write(&part, b"somebody else's half-written file");

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::CompletedWithFailures);
    assert_eq!(
        read(&part),
        b"somebody else's half-written file",
        "the other job's file was overwritten"
    );
    assert!(!dst.join("data.bin").exists());
}

#[test]
fn a_name_windows_would_mangle_is_reported_rather_than_silently_changed() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    write(&src.join("notes."), b"trailing dot");
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert!(ran.warned(WarningKind::NameHazard), "{:?}", ran.warnings);
    if !cfg!(windows) {
        // On Unix it is a perfectly ordinary name and the copy is correct.
        assert_eq!(read(&dst.join("src/notes.")), b"trailing dot");
    }
}

// ---- conflicts -----------------------------------------------------------

fn two_trees_that_collide(t: &TempDir, n: usize) -> (PathBuf, PathBuf) {
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    for i in 0..n {
        write(&src.join(format!("f{i}.txt")), b"new");
        write(&dst.join(format!("f{i}.txt")), b"old");
    }
    (src, dst)
}

#[test]
fn overwrite_replaces_and_skip_does_not() {
    let t = TempDir::new().unwrap();
    let (src, dst) = two_trees_that_collide(&t, 1);
    let file = src.join("f0.txt");

    let ran = drive(copy(&[&file], &dst), Options::default(), |_, _| {
        Some(Resolution::once(ConflictChoice::Skip))
    });
    assert_eq!(ran.conflicts.len(), 1);
    assert_eq!(read(&dst.join("f0.txt")), b"old");
    assert_eq!(ran.outcome().skipped, 1);

    let ran = drive(copy(&[&file], &dst), Options::default(), |_, _| {
        Some(Resolution::once(ConflictChoice::Overwrite))
    });
    assert_eq!(ran.status(), &Status::Completed);
    assert_eq!(read(&dst.join("f0.txt")), b"new");
}

/// The dialog has to name both files, or the user is answering blind.
#[test]
fn a_conflict_describes_both_sides() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/x.txt");
    let dst = t.path().join("dst");
    write(&src, b"1234567890");
    write(&dst.join("x.txt"), b"ab");

    let ran = drive(copy(&[&src], &dst), Options::default(), |_, _| {
        Some(Resolution::once(ConflictChoice::Skip))
    });
    let c = &ran.conflicts[0];
    assert_eq!(c.source.size, Some(10));
    assert_eq!(c.destination.size, Some(2));
    assert!(c.source.modified.is_some() && c.destination.modified.is_some());
    assert_eq!(c.kind, ConflictKind::SameKind);
}

#[test]
fn one_answer_covers_the_rest_of_the_files() {
    let t = TempDir::new().unwrap();
    let (src, dst) = two_trees_that_collide(&t, 8);
    let files: Vec<PathBuf> = (0..8).map(|i| src.join(format!("f{i}.txt"))).collect();
    let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();

    let ran = drive(copy(&refs, &dst), Options::default(), |_, _| {
        Some(Resolution::all(ConflictChoice::Overwrite))
    });
    assert_eq!(ran.conflicts.len(), 1, "asked more than once");
    for i in 0..8 {
        assert_eq!(read(&dst.join(format!("f{i}.txt"))), b"new");
    }
}

/// One typed name applied to eight files would leave one file and seven
/// overwrites of it, reported as eight successes.
#[test]
fn a_typed_rename_is_never_stretched_across_the_selection() {
    let t = TempDir::new().unwrap();
    let (src, dst) = two_trees_that_collide(&t, 4);
    let files: Vec<PathBuf> = (0..4).map(|i| src.join(format!("f{i}.txt"))).collect();
    let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();

    let ran = drive(copy(&refs, &dst), Options::default(), |_, n| {
        Some(Resolution {
            choice: ConflictChoice::RenameTo(format!("renamed{n}.txt")),
            apply_to_all: true, // asked for, and deliberately not honoured
        })
    });
    assert_eq!(
        ran.conflicts.len(),
        4,
        "a typed name must be asked for every time"
    );
    for n in 0..4 {
        assert_eq!(read(&dst.join(format!("renamed{n}.txt"))), b"new");
    }
    for i in 0..4 {
        assert_eq!(read(&dst.join(format!("f{i}.txt"))), b"old");
    }
}

/// The "apply to all" is the user's, not ours: taking it back has to bring the
/// question straight back.
///
/// A big file sits between the first collision and the next one, so the engine
/// is demonstrably still busy when the standing choice is revoked — the test is
/// not racing the job to get there first.
#[test]
fn a_standing_choice_can_be_revoked_mid_job() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    for name in ["c0.txt", "c1.txt", "c2.txt"] {
        write(&src.join(name), b"new");
        write(&dst.join(name), b"old");
    }
    write(&src.join("big.bin"), &vec![b'w'; 24 * 1024 * 1024]);

    let job = Job::Copy {
        sources: vec![
            src.join("c0.txt"),
            src.join("big.bin"),
            src.join("c1.txt"),
            src.join("c2.txt"),
        ],
        destination: dst.clone(),
    };
    let opts = Options {
        reflink: false,
        ..Options::default()
    };
    let (handle, mut rx) = spawn(job, opts).unwrap();
    let mut asked = 0;
    while let Some(event) = rx.blocking_recv() {
        if let JobEvent::Conflict(prompt) = event {
            asked += 1;
            if asked > 1 {
                // Already proved the point; answer for this file only.
                prompt.answer(Resolution::once(ConflictChoice::Skip));
                continue;
            }
            prompt.answer(Resolution::all(ConflictChoice::Skip));
            // Wait for the engine to record the standing choice, then take it
            // back — all while it is still copying the 24MB file that sits
            // between this collision and the next.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while handle.standing_choice().is_none() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                handle.standing_choice().is_some(),
                "the answer did not stand"
            );
            handle.revoke_standing_choice();
            assert_eq!(handle.standing_choice(), None);
        }
    }
    assert_eq!(
        asked, 3,
        "the collision after the revocation had to be asked about again"
    );
}

#[test]
fn auto_rename_puts_the_copy_next_to_the_original() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/photo.jpg");
    let dst = t.path().join("dst");
    write(&src, b"new");
    write(&dst.join("photo.jpg"), b"old");

    let ran = drive(copy(&[&src], &dst), Options::default(), |_, _| {
        Some(Resolution::once(ConflictChoice::AutoRename))
    });
    assert_eq!(ran.status(), &Status::Completed);
    assert_eq!(read(&dst.join("photo.jpg")), b"old");
    assert_eq!(read(&dst.join("photo (2).jpg")), b"new");
}

#[test]
fn newer_only_leaves_a_newer_destination_alone() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/x.txt");
    let dst = t.path().join("dst");
    write(&src, b"stale");
    write(&dst.join("x.txt"), b"fresh");
    let old = filetime::FileTime::from_unix_time(1_000_000_000, 0);
    let new = filetime::FileTime::from_unix_time(2_000_000_000, 0);
    filetime::set_file_times(&src, old, old).unwrap();
    filetime::set_file_times(dst.join("x.txt"), new, new).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), |_, _| {
        Some(Resolution::all(ConflictChoice::OverwriteIfNewer))
    });
    assert_eq!(read(&dst.join("x.txt")), b"fresh");
    assert_eq!(ran.outcome().skipped, 1);
}

/// Saying "overwrite everything" about files must never, later in the same job,
/// silently delete a directory tree.
#[test]
fn a_standing_overwrite_does_not_cover_a_file_landing_on_a_directory() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    write(&src.join("a.txt"), b"new");
    write(&src.join("b"), b"i am a file");
    let dst = t.path().join("dst");
    write(&dst.join("a.txt"), b"old");
    write(&dst.join("b/inside.txt"), b"a whole directory");

    let files = [src.join("a.txt"), src.join("b")];
    let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
    let ran = drive(copy(&refs, &dst), Options::default(), |c, _| {
        match c.kind {
            ConflictKind::SameKind => Some(Resolution::all(ConflictChoice::Overwrite)),
            // The dangerous one is asked separately, and here it is refused.
            ConflictKind::KindMismatch => Some(Resolution::once(ConflictChoice::Skip)),
        }
    });
    assert_eq!(ran.conflicts.len(), 2, "the mismatch was asked on its own");
    assert!(
        ran.conflicts
            .iter()
            .any(|c| c.kind == ConflictKind::KindMismatch)
    );
    assert_eq!(read(&dst.join("a.txt")), b"new");
    assert_eq!(
        read(&dst.join("b/inside.txt")),
        b"a whole directory",
        "a standing yes must not have removed this tree"
    );
}

#[test]
fn two_directories_merge_without_a_question() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/shared");
    write(&src.join("new.txt"), b"new");
    let dst = t.path().join("dst");
    write(&dst.join("shared/existing.txt"), b"existing");

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed);
    assert_eq!(read(&dst.join("shared/existing.txt")), b"existing");
    assert_eq!(read(&dst.join("shared/new.txt")), b"new");
}

/// The dialog closed and nobody answered. Guessing would be the whole bug.
#[test]
fn an_unanswered_conflict_stops_the_job_and_changes_nothing() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/x.txt");
    let dst = t.path().join("dst");
    write(&src, b"new");
    write(&dst.join("x.txt"), b"old");

    let ran = drive(copy(&[&src], &dst), Options::default(), |_, _| None);
    assert!(
        matches!(ran.status(), Status::Aborted(_)),
        "{:?}",
        ran.status()
    );
    assert_eq!(read(&dst.join("x.txt")), b"old");
    assert!(leftover_parts(t.path()).is_empty());
}

#[test]
fn abort_stops_the_job_and_keeps_what_was_already_done() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    write(&src.join("a.txt"), b"new-a");
    write(&src.join("b.txt"), b"new-b");
    let dst = t.path().join("dst");
    write(&dst.join("a.txt"), b"old-a");
    write(&dst.join("b.txt"), b"old-b");
    let files = [src.join("a.txt"), src.join("b.txt")];
    let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();

    let ran = drive(copy(&refs, &dst), Options::default(), |_, n| {
        Some(Resolution::once(if n == 0 {
            ConflictChoice::Overwrite
        } else {
            ConflictChoice::Abort
        }))
    });
    assert!(matches!(ran.status(), Status::Aborted(_)));
    assert_eq!(read(&dst.join("a.txt")), b"new-a", "the first one finished");
    assert_eq!(
        read(&dst.join("b.txt")),
        b"old-b",
        "the second never started"
    );
}

// ---- move ----------------------------------------------------------------

#[test]
fn a_move_within_one_filesystem_leaves_nothing_behind() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/tree");
    write(&src.join("a/deep.txt"), b"deep");
    write(&src.join("top.txt"), b"top");
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(move_job(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    assert_eq!(read(&dst.join("tree/a/deep.txt")), b"deep");
    assert_eq!(read(&dst.join("tree/top.txt")), b"top");
    assert!(!src.exists(), "the source is gone");
    // The whole tree went across in one `rename`, so nothing walked it — the
    // bar still has to reach the end, from what the scan counted.
    assert_eq!(ran.outcome().files_done, 2);
    assert_eq!(ran.last_progress().percent(), Some(100.0));
}

#[test]
fn a_move_across_filesystems_copies_verifies_and_only_then_deletes() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/tree");
    let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    write(&src.join("big.bin"), &payload);
    write(&src.join("a/small.txt"), b"small");
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(move_job(&[&src], &dst), cross_device(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    assert_eq!(read(&dst.join("tree/big.bin")), payload);
    assert_eq!(read(&dst.join("tree/a/small.txt")), b"small");
    assert!(!src.exists(), "the sources are removed once verified");
    assert!(leftover_parts(t.path()).is_empty());
}

/// Nothing is deleted on the strength of a copy that did not happen.
#[cfg(unix)]
#[test]
fn a_move_that_cannot_write_keeps_every_original() {
    use std::os::unix::fs::PermissionsExt;
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    write(&src.join("a.txt"), b"precious");
    write(&src.join("b.txt"), b"also precious");
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();
    fs::set_permissions(&dst, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::File::create(dst.join("probe")).is_ok() {
        // Running as root: the read-only bit is not enforced and proves nothing.
        let _ = fs::remove_file(dst.join("probe"));
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let ran = drive(move_job(&[&src], &dst), cross_device(), no_conflicts);
    assert_ne!(ran.status(), &Status::Completed);
    assert_eq!(read(&src.join("a.txt")), b"precious");
    assert_eq!(read(&src.join("b.txt")), b"also precious");
    fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A skipped file means its directory survives too — `remove_dir`, never
/// `remove_dir_all`.
#[test]
fn a_move_that_skips_a_file_keeps_the_file_and_its_directory() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/tree");
    write(&src.join("keep.txt"), b"new");
    write(&src.join("move.txt"), b"moved");
    let dst = t.path().join("dst");
    write(&dst.join("tree/keep.txt"), b"old");

    let ran = drive(move_job(&[&src], &dst), cross_device(), |_, _| {
        Some(Resolution::once(ConflictChoice::Skip))
    });
    assert_eq!(read(&dst.join("tree/move.txt")), b"moved");
    assert_eq!(read(&dst.join("tree/keep.txt")), b"old");
    assert_eq!(
        read(&src.join("keep.txt")),
        b"new",
        "a file that was not copied must not be deleted"
    );
    assert!(src.is_dir(), "and the directory holding it stays");
    assert!(ran.warned(WarningKind::SourceKept), "{:?}", ran.warnings);
}

#[test]
fn a_move_keeps_the_modification_time() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/x.bin");
    write(&src, b"payload");
    let past = filetime::FileTime::from_unix_time(1_234_567_890, 0);
    filetime::set_file_times(&src, past, past).unwrap();
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    drive(move_job(&[&src], &dst), cross_device(), no_conflicts);
    let meta = dst.join("x.bin").symlink_metadata().unwrap();
    assert_eq!(filetime::FileTime::from_last_modification_time(&meta), past);
}

#[test]
fn a_move_is_never_allowed_to_verify_less_than_a_hash() {
    use super::engine::required_verify;
    assert_eq!(required_verify(Verify::None, true), Verify::Hash);
    assert_eq!(required_verify(Verify::Size, true), Verify::Hash);
    assert_eq!(required_verify(Verify::None, false), Verify::None);
}

// ---- delete --------------------------------------------------------------

#[derive(Debug, Default)]
struct FakeTrash {
    calls: Mutex<Vec<PathBuf>>,
    refuse: bool,
}

impl TrashCan for FakeTrash {
    fn trash(&self, path: &Path) -> std::result::Result<(), String> {
        if let Ok(mut c) = self.calls.lock() {
            c.push(path.to_path_buf());
        }
        if self.refuse {
            Err("this volume has no trash".into())
        } else {
            Ok(())
        }
    }
}

fn with_trash(t: Arc<FakeTrash>) -> Options {
    Options {
        trash: Some(t),
        ..Options::default()
    }
}

#[test]
fn deleting_defaults_to_the_trash() {
    let t = TempDir::new().unwrap();
    let victim = t.path().join("victim.txt");
    write(&victim, b"recoverable");
    let bin = Arc::new(FakeTrash::default());

    let ran = drive(
        Job::Delete {
            targets: vec![victim.clone()],
            mode: DeleteMode::default(),
        },
        with_trash(bin.clone()),
        no_conflicts,
    );
    assert_eq!(ran.status(), &Status::Completed);
    assert_eq!(bin.calls.lock().unwrap().as_slice(), &[victim]);
}

/// The worst thing this program could do: turn "I can get that back" into "it
/// is gone" because the recoverable path was unavailable.
#[test]
fn a_trash_that_refuses_deletes_nothing_at_all() {
    let t = TempDir::new().unwrap();
    let victim = t.path().join("victim.txt");
    write(&victim, b"still here");
    let bin = Arc::new(FakeTrash {
        refuse: true,
        ..FakeTrash::default()
    });

    let ran = drive(
        Job::Delete {
            targets: vec![victim.clone()],
            mode: DeleteMode::Trash,
        },
        with_trash(bin),
        no_conflicts,
    );
    assert_eq!(
        read(&victim),
        b"still here",
        "a refused trash must never fall through to a permanent delete"
    );
    assert_ne!(ran.status(), &Status::Completed);
    assert!(
        ran.warned(WarningKind::TrashUnavailable),
        "{:?}",
        ran.warnings
    );
}

#[test]
fn a_permanent_delete_removes_the_whole_tree() {
    let t = TempDir::new().unwrap();
    let tree = t.path().join("tree");
    write(&tree.join("a/b/c.txt"), b"c");
    write(&tree.join("a/d.txt"), b"d");
    write(&tree.join("e.txt"), b"e");

    let ran = drive(
        Job::Delete {
            targets: vec![tree.clone()],
            mode: DeleteMode::Permanent,
        },
        Options::default(),
        no_conflicts,
    );
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    assert!(!tree.exists());
    assert_eq!(ran.outcome().files_done, 3);
}

/// The catastrophic one. A link inside the tree must lose the link and nothing
/// on the other end of it.
#[cfg(unix)]
#[test]
fn deleting_a_tree_never_follows_a_link_out_of_it() {
    let t = TempDir::new().unwrap();
    let outside = t.path().join("outside");
    write(&outside.join("irreplaceable.txt"), b"a wedding photo album");
    let tree = t.path().join("tree");
    write(&tree.join("ordinary.txt"), b"ordinary");
    std::os::unix::fs::symlink(&outside, tree.join("escape")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("irreplaceable.txt"),
        tree.join("escape-to-file"),
    )
    .unwrap();

    let ran = drive(
        Job::Delete {
            targets: vec![tree.clone()],
            mode: DeleteMode::Permanent,
        },
        Options::default(),
        no_conflicts,
    );
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    assert!(!tree.exists(), "the tree itself is gone, links and all");
    assert_eq!(
        read(&outside.join("irreplaceable.txt")),
        b"a wedding photo album",
        "the delete walked out of the tree it was pointed at"
    );
}

#[test]
fn a_filesystem_root_is_refused_before_anything_is_unlinked() {
    let ran = drive(
        Job::Delete {
            targets: vec![PathBuf::from("/")],
            mode: DeleteMode::Permanent,
        },
        Options::default(),
        no_conflicts,
    );
    assert!(
        matches!(ran.status(), Status::Aborted(_)),
        "{:?}",
        ran.status()
    );
    assert_eq!(ran.outcome().files_done, 0);
}

#[test]
fn an_empty_delete_is_refused_rather_than_reported_as_a_success() {
    let ran = drive(
        Job::Delete {
            targets: vec![],
            mode: DeleteMode::Permanent,
        },
        Options::default(),
        no_conflicts,
    );
    assert!(matches!(ran.status(), Status::Aborted(_)));
}

// ---- rename and make-directory -------------------------------------------

/// On a case-insensitive filesystem the destination of this rename *is* the
/// source. Treating it as a conflict and overwriting would delete the file.
#[test]
fn renaming_only_the_case_keeps_the_file() {
    let t = TempDir::new().unwrap();
    let lower = t.path().join("notes.txt");
    write(&lower, b"the only copy");

    let ran = drive(
        Job::Rename {
            path: lower.clone(),
            new_name: "NOTES.TXT".into(),
        },
        Options::default(),
        // If this asks, the identity check failed and the answer would decide
        // whether the file survives.
        no_conflicts,
    );
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    let renamed = t.path().join("NOTES.TXT");
    assert_eq!(read(&renamed), b"the only copy");
}

#[test]
fn a_rename_onto_an_existing_name_asks_first() {
    let t = TempDir::new().unwrap();
    let a = t.path().join("a.txt");
    let b = t.path().join("b.txt");
    write(&a, b"a");
    write(&b, b"b");

    let ran = drive(
        Job::Rename {
            path: a.clone(),
            new_name: "b.txt".into(),
        },
        Options::default(),
        |_, _| Some(Resolution::once(ConflictChoice::Skip)),
    );
    assert_eq!(ran.conflicts.len(), 1);
    assert_eq!(read(&a), b"a");
    assert_eq!(read(&b), b"b");
}

#[test]
fn a_rename_is_a_name_and_never_a_path() {
    let t = TempDir::new().unwrap();
    let f = t.path().join("here.txt");
    write(&f, b"here");

    for attempt in ["../escaped.txt", "sub/inside.txt", "..", ""] {
        let ran = drive(
            Job::Rename {
                path: f.clone(),
                new_name: attempt.into(),
            },
            Options::default(),
            no_conflicts,
        );
        assert!(
            matches!(ran.status(), Status::Aborted(_)),
            "`{attempt}` was accepted"
        );
    }
    assert_eq!(read(&f), b"here");
    assert!(!t.path().join("../escaped.txt").exists());
}

#[test]
fn make_directory_creates_every_level_asked_for() {
    let t = TempDir::new().unwrap();
    let ran = drive(
        Job::MakeDirectory {
            parent: t.path().to_path_buf(),
            name: "a/b/c".into(),
        },
        Options::default(),
        no_conflicts,
    );
    assert_eq!(ran.status(), &Status::Completed);
    assert!(t.path().join("a/b/c").is_dir());
}

#[test]
fn make_directory_refuses_to_leave_its_parent() {
    let t = TempDir::new().unwrap();
    let inner = t.path().join("inner");
    fs::create_dir_all(&inner).unwrap();
    for attempt in ["../outside", "/absolute", "a/../../b"] {
        let ran = drive(
            Job::MakeDirectory {
                parent: inner.clone(),
                name: attempt.into(),
            },
            Options::default(),
            no_conflicts,
        );
        assert!(
            matches!(ran.status(), Status::Aborted(_)),
            "`{attempt}` was accepted"
        );
    }
    assert!(!t.path().join("outside").exists());
}

#[test]
fn make_directory_says_so_when_the_name_is_taken() {
    let t = TempDir::new().unwrap();
    write(&t.path().join("taken"), b"a file, actually");
    let ran = drive(
        Job::MakeDirectory {
            parent: t.path().to_path_buf(),
            name: "taken".into(),
        },
        Options::default(),
        no_conflicts,
    );
    assert!(matches!(ran.status(), Status::Aborted(_)));
    assert_eq!(read(&t.path().join("taken")), b"a file, actually");
}

// ---- cancellation --------------------------------------------------------

#[test]
fn a_cancelled_copy_leaves_only_complete_files() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    let chunk = vec![b'z'; 12 * 1024 * 1024];
    for i in 0..6 {
        write(&src.join(format!("big{i}.bin")), &chunk);
    }
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let opts = Options {
        // A reflink would finish the whole file in one instant and there would
        // be nothing to cancel in the middle of.
        reflink: false,
        ..Options::default()
    };
    let ran = drive_cancelling(copy(&[&src], &dst), opts, |p| {
        p.phase == Phase::Transferring
    });

    assert_eq!(ran.status(), &Status::Cancelled);
    assert!(
        leftover_parts(t.path()).is_empty(),
        "a graceful cancel cleans up its own in-flight file"
    );
    // Whatever arrived is whole.
    if let Ok(rd) = fs::read_dir(dst.join("src")) {
        for e in rd.flatten() {
            assert_eq!(
                e.metadata().unwrap().len(),
                chunk.len() as u64,
                "{} is a partial file wearing a real name",
                e.path().display()
            );
        }
    }
}

#[test]
fn a_cancelled_move_leaves_every_unmoved_source_intact() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    let chunk = vec![b'q'; 8 * 1024 * 1024];
    for i in 0..6 {
        write(&src.join(format!("f{i}.bin")), &chunk);
    }
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let opts = cross_device();
    let ran = drive_cancelling(move_job(&[&src], &dst), opts, |p| {
        p.phase == Phase::Transferring
    });
    assert_eq!(ran.status(), &Status::Cancelled);

    // Every file is in exactly one of the two places, and whole in both.
    for i in 0..6 {
        let name = format!("f{i}.bin");
        let a = src.join(&name);
        let b = dst.join("src").join(&name);
        assert!(
            a.exists() || b.exists(),
            "{name} is in neither place — it was lost"
        );
        for p in [a, b] {
            if let Ok(m) = p.symlink_metadata() {
                assert_eq!(m.len(), chunk.len() as u64, "{} is truncated", p.display());
            }
        }
    }
    assert!(leftover_parts(t.path()).is_empty());
}

/// Esc has to work while a dialog is open, or a job with a pending question is
/// a job nobody can stop.
#[test]
fn a_job_can_be_cancelled_while_it_waits_for_an_answer() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/x.txt");
    let dst = t.path().join("dst");
    write(&src, b"new");
    write(&dst.join("x.txt"), b"old");

    let (handle, mut rx) = spawn(copy(&[&src], &dst), Options::default()).unwrap();
    let mut held = None;
    let mut outcome = None;
    while let Some(event) = rx.blocking_recv() {
        match event {
            JobEvent::Conflict(prompt) => {
                handle.cancel();
                // Deliberately keep the prompt: the engine must notice the
                // cancellation on its own rather than waiting for an answer.
                held = Some(prompt);
            }
            JobEvent::Finished(o) => outcome = Some(o),
            _ => {}
        }
    }
    assert!(held.is_some());
    assert_eq!(outcome.map(|o| o.status), Some(Status::Cancelled));
    assert_eq!(read(&dst.join("x.txt")), b"old");
}

// ---- the ugly cases ------------------------------------------------------

/// The scan's total is a measurement, not a promise. A file that grew after it
/// was counted delivers more bytes than were expected, and the bar must not run
/// off the end.
#[test]
fn a_source_that_grows_after_the_scan_is_still_copied_whole() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    write(&src.join("a.txt"), b"asks a question");
    write(&src.join("b.bin"), &vec![b'g'; 100_000]);
    let dst = t.path().join("dst");
    write(&dst.join("a.txt"), b"in the way");

    let files = [src.join("a.txt"), src.join("b.bin")];
    let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
    let grown = src.join("b.bin");

    let ran = drive(copy(&refs, &dst), Options::default(), |_, _| {
        // The conflict on a.txt is the synchronisation point: the scan has
        // finished, the transfer has not reached b.bin yet.
        let mut f = fs::OpenOptions::new().append(true).open(&grown).unwrap();
        use std::io::Write as _;
        f.write_all(&vec![b'h'; 400_000]).unwrap();
        f.sync_all().unwrap();
        Some(Resolution::once(ConflictChoice::Skip))
    });

    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    assert_eq!(
        read(&dst.join("b.bin")).len(),
        500_000,
        "the copy is of the file as it was when the copy started, entire"
    );
    let last = ran.last_progress();
    assert!(last.bytes_done > last.bytes_total.unwrap_or(0));
    assert_eq!(
        last.percent(),
        Some(100.0),
        "a wrong denominator must not produce a bar past full"
    );
}

#[test]
fn a_sparse_file_survives_the_round_trip_byte_for_byte() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src/disk.img");
    fs::create_dir_all(src.parent().unwrap()).unwrap();
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = fs::File::create(&src).unwrap();
        f.write_all(b"header").unwrap();
        f.seek(SeekFrom::Start(8 * 1024 * 1024)).unwrap();
        f.write_all(b"footer").unwrap();
        f.sync_all().unwrap();
    }
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let opts = Options {
        reflink: false, // exercise the hole-aware streaming path itself
        verify: Verify::Hash,
        ..Options::default()
    };
    let ran = drive(copy(&[&src], &dst), opts, no_conflicts);
    assert_eq!(ran.status(), &Status::Completed, "{:?}", ran.failures);
    assert_eq!(
        read(&dst.join("disk.img")),
        read(&src),
        "holes must read back as the zeros they are"
    );
}

#[test]
fn nothing_a_job_writes_is_ever_visible_under_a_real_name_until_it_is_whole() {
    let t = TempDir::new().unwrap();
    let src = t.path().join("src");
    for i in 0..20 {
        write(&src.join(format!("f{i}.bin")), &vec![b'k'; 50_000]);
    }
    let dst = t.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    let ran = drive(copy(&[&src], &dst), Options::default(), no_conflicts);
    assert_eq!(ran.status(), &Status::Completed);
    assert!(leftover_parts(t.path()).is_empty());
    for i in 0..20 {
        assert_eq!(read(&dst.join(format!("src/f{i}.bin"))).len(), 50_000);
    }
}
