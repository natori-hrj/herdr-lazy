//! Generated lockfile families, on a trusted, stable local parent directory.
//!
//! Cooperating readers and publishers hold the same OS lock from observation through snapshot
//! capture or the last publication/rollback. It does not cover Herdr operations, external
//! writers, or parent-directory changes. Identity checks catch some external path replacements,
//! not in-place edits or changes between the last check and a filesystem operation.
//! This is not a crash transaction: interruption can leave partial history or temporary files.
//! Unix locks the directory inode, preserves ordinary permission bits and rejects special mode
//! bits and extended ACLs on publication.
//! Windows keeps a guard file in the directory. New files (including the guard) get the
//! directory's normal file creation policy. Existing sources must match the stage's owner
//! and DACL before any payload is written, or publication is refused. This does not copy
//! ACLs or preserve other Windows metadata.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{lock_temp_path, read_limited, validate_local_text_size, MAX_LOCAL_INPUT_BYTES};

pub(crate) const MAX_PREVIOUS_LOCKFILES: usize = 3;

pub(crate) fn lock_history_beside(lock: &Path, generation: usize) -> PathBuf {
    assert!((1..=MAX_PREVIOUS_LOCKFILES).contains(&generation));
    let mut name = OsString::from(lock.file_name().expect("generated lock filename"));
    name.push(format!(".{generation}"));
    lock.with_file_name(name)
}

fn invalid(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("plugin lock {}: {reason}", path.display()),
    )
}

struct Leaf {
    file: File,
    id: platform::Identity,
}

impl Leaf {
    fn open(path: &Path) -> io::Result<Option<Self>> {
        platform::open(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let bytes = read_limited(&self.file, MAX_LOCAL_INPUT_BYTES)?
            .ok_or_else(|| invalid(path, &format!("exceeds {MAX_LOCAL_INPUT_BYTES} bytes")))?;
        if platform::identity(&self.file, path)? != self.id {
            return Err(invalid(path, "file identity changed during observation"));
        }
        revalidate(path, Some(self.id))?;
        Ok(bytes)
    }
}

fn revalidate(path: &Path, expected: Option<platform::Identity>) -> io::Result<()> {
    if platform::path_identity(path)? != expected {
        return Err(invalid(path, "file identity changed"));
    }
    Ok(())
}

fn family(lock: &Path) -> io::Result<Vec<(PathBuf, Option<Leaf>)>> {
    let paths = std::iter::once(lock.to_owned())
        .chain((1..=MAX_PREVIOUS_LOCKFILES).map(|n| lock_history_beside(lock, n)));
    let mut family: Vec<(PathBuf, Option<Leaf>)> = Vec::new();
    for path in paths {
        let leaf = Leaf::open(&path)?;
        if let Some(leaf) = &leaf {
            if family
                .iter()
                .any(|(_, old)| old.as_ref().is_some_and(|old| old.id == leaf.id))
            {
                return Err(invalid(&path, "shares identity with another lock leaf"));
            }
        }
        family.push((path, leaf));
    }
    Ok(family)
}

fn revalidate_family(family: &[(PathBuf, Option<Leaf>)]) -> io::Result<()> {
    for (path, leaf) in family {
        revalidate(path, leaf.as_ref().map(|leaf| leaf.id))?;
    }
    Ok(())
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn read_text(lock: &Path, previous: bool) -> io::Result<String> {
    platform::read_guarded(parent(lock), || read_snapshot(lock, previous))
}

fn read_snapshot(lock: &Path, previous: bool) -> io::Result<String> {
    let family = family(lock)?;
    let (path, leaf) = &family[usize::from(previous)];
    let bytes = leaf.as_ref().map(|leaf| leaf.read(path)).transpose()?;
    revalidate_family(&family)?;
    let bytes = bytes.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no lock at {}", path.display()),
        )
    })?;
    String::from_utf8(bytes).map_err(|_| invalid(path, "not valid UTF-8"))
}

struct Stage {
    path: PathBuf,
    id: platform::Identity,
    #[cfg(unix)]
    file: File,
    #[cfg(unix)]
    mode: u32,
}

impl Stage {
    fn new(destination: &Path, bytes: &[u8], source: Option<&Leaf>) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        platform::stage_options(&mut options);
        #[cfg(unix)]
        let mode = platform::stage_mode(source, destination)?;
        let (path, mut file) = loop {
            let path = lock_temp_path(destination);
            match options.open(&path) {
                Ok(file) => break (path, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let id = match platform::identity(&file, &path) {
            Ok(id) => id,
            Err(error) => {
                drop(file);
                let _ = fs::remove_file(path);
                return Err(error);
            }
        };
        #[cfg(windows)]
        let written = platform::check_stage_policy(&file, source, destination)
            .and_then(|()| file.write_all(bytes));
        #[cfg(windows)]
        drop(file);
        #[cfg(unix)]
        let written =
            platform::check_private_stage(&file, &path).and_then(|()| file.write_all(bytes));
        let stage = Self {
            path,
            id,
            #[cfg(unix)]
            file,
            #[cfg(unix)]
            mode,
        };
        written?;
        Ok(stage)
    }

    #[cfg(unix)]
    fn prepare(&self) -> io::Result<()> {
        platform::apply_stage_mode(&self.file, &self.path, self.mode)
    }

    fn publish(&self, path: &Path, expected: Option<platform::Identity>) -> io::Result<()> {
        revalidate(&self.path, Some(self.id))?;
        revalidate(path, expected)?;
        platform::publish(&self.path, path, expected.is_some())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct Change {
    path: PathBuf,
    old: Option<platform::Identity>,
    forward: Option<Stage>,
    rollback: Option<Stage>,
}

impl Change {
    fn apply(&self, stage: Option<&Stage>, expected: Option<platform::Identity>) -> io::Result<()> {
        if let Some(stage) = stage {
            stage.publish(&self.path, expected)
        } else {
            revalidate(&self.path, expected)?;
            if expected.is_some() {
                fs::remove_file(&self.path)?;
            }
            Ok(())
        }
    }
}

fn commit(
    changes: &[Change],
    lock: &Path,
    active: &Stage,
    current: Option<platform::Identity>,
) -> io::Result<()> {
    // Payloads were written privately. Apply and verify the final Unix permissions on every
    // stage now, including rollback copies, so a policy failure can't leave partial history.
    #[cfg(unix)]
    {
        active.prepare()?;
        for change in changes {
            for stage in [&change.forward, &change.rollback].into_iter().flatten() {
                stage.prepare()?;
            }
        }
    }
    let mut applied = 0;
    let result = (|| {
        for change in changes {
            change.apply(change.forward.as_ref(), change.old)?;
            applied += 1;
        }
        active.publish(lock, current)
    })();
    if let Err(error) = result {
        let mut rollback_error = None;
        for change in changes[..applied].iter().rev() {
            let expected = change.forward.as_ref().map(|stage| stage.id);
            if let Err(error) = change.apply(change.rollback.as_ref(), expected) {
                rollback_error.get_or_insert(error);
            }
        }
        if let Some(rollback) = rollback_error {
            return Err(io::Error::other(format!(
                "lock publication failed: {error}; history rollback failed: {rollback}"
            )));
        }
        return Err(error);
    }
    Ok(())
}

/// Return whether an active file was published. Even on first publication or a no-op, inspect
/// all generated leaves before deciding what to do. Stage forward and rollback payloads before
/// touching old files; both use the bounded bytes captured from the original handles.
pub(crate) fn write_lock_contents(lock: &Path, body: &str) -> io::Result<bool> {
    validate_local_text_size(body.as_bytes(), "plugin lock", lock)?;
    let _guard = platform::guard(parent(lock))?;
    let family = family(lock)?;
    let bytes: Vec<_> = family
        .iter()
        .map(|(path, leaf)| leaf.as_ref().map(|leaf| leaf.read(path)).transpose())
        .collect::<io::Result<_>>()?;
    let current = family[0].1.as_ref();
    let unchanged = bytes[0].as_deref() == Some(body.as_bytes());
    if unchanged {
        revalidate_family(&family)?;
        return Ok(false);
    }

    let active = Stage::new(lock, body.as_bytes(), current)?;
    let mut changes = Vec::new();
    if current.is_some() {
        for index in (1..family.len()).rev() {
            let (path, old) = &family[index];
            let stage = |source: usize| {
                bytes[source]
                    .as_deref()
                    .map(|body| Stage::new(path, body, family[source].1.as_ref()))
                    .transpose()
            };
            changes.push(Change {
                path: path.clone(),
                old: old.as_ref().map(|leaf| leaf.id),
                forward: stage(index - 1)?,
                rollback: stage(index)?,
            });
        }
    }
    revalidate_family(&family)?;
    let current = current.map(|leaf| leaf.id);
    // Classic Windows replacement can reject an open destination even with delete sharing.
    // Keep the captured handles through observation and staging, then close them. Each
    // publication still opens fresh no-follow handles to revalidate IDs immediately beforehand.
    #[cfg(windows)]
    drop(family);
    commit(&changes, lock, &active, current)?;
    Ok(true)
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    pub(super) type Identity = (u64, u64);

    pub(super) fn guard(parent: &Path) -> io::Result<File> {
        open_guard(parent, libc::LOCK_EX)
    }

    fn open_guard(parent: &Path, flags: libc::c_int) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(parent)?;
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), flags) } == 0 {
                return Ok(file);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub(super) fn read_guarded<T>(
        parent: &Path,
        read: impl Fn() -> io::Result<T>,
    ) -> io::Result<T> {
        // Details and restore must not consume tentative history or wait on a writer.
        let _guard = open_guard(parent, libc::LOCK_SH | libc::LOCK_NB)?;
        read()
    }

    fn identify(metadata: fs::Metadata, path: &Path) -> io::Result<Identity> {
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.ino() == 0 {
            return Err(invalid(
                path,
                "not an identifiable independent regular file",
            ));
        }
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(super) fn identity(file: &File, path: &Path) -> io::Result<Identity> {
        identify(file.metadata()?, path)
    }

    pub(super) fn path_identity(path: &Path) -> io::Result<Option<Identity>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => identify(metadata, path).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn open(path: &Path) -> io::Result<Option<Leaf>> {
        let Some(id) = path_identity(path)? else {
            return Ok(None);
        };
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    invalid(path, "file disappeared while opening")
                } else {
                    error
                }
            })?;
        if identity(&file, path)? != id {
            return Err(invalid(path, "file identity changed while opening"));
        }
        Ok(Some(Leaf { file, id }))
    }

    pub(super) fn stage_options(options: &mut OpenOptions) {
        options.mode(PRIVATE_MODE);
    }

    const MODE_BITS: u32 = 0o7777;
    const SPECIAL_BITS: u32 = 0o7000;
    const PRIVATE_MODE: u32 = 0o600;

    fn mode(file: &File) -> io::Result<u32> {
        Ok(file.metadata()?.permissions().mode() & MODE_BITS)
    }

    pub(super) fn stage_mode(source: Option<&Leaf>, path: &Path) -> io::Result<u32> {
        let Some(source) = source else {
            return Ok(PRIVATE_MODE);
        };
        check_stage_policy(&source.file, path)?;
        let mode = mode(&source.file)?;
        if mode & SPECIAL_BITS != 0 {
            return Err(invalid(
                path,
                "special mode bits are unsupported for lock publication",
            ));
        }
        Ok(mode)
    }

    pub(super) fn check_private_stage(file: &File, path: &Path) -> io::Result<()> {
        check_stage_policy(file, path)?;
        if mode(file)? & !PRIVATE_MODE != 0 {
            return Err(invalid(path, "stage mode is not private"));
        }
        Ok(())
    }

    pub(super) fn apply_stage_mode(file: &File, path: &Path, expected: u32) -> io::Result<()> {
        file.set_permissions(fs::Permissions::from_mode(expected))?;
        if mode(file)? != expected {
            return Err(invalid(path, "stage cannot represent the source mode"));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub(super) fn check_stage_policy(file: &File, path: &Path) -> io::Result<()> {
        extern "C" {
            fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
            fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
        }
        // macOS ACLs can grant access independently of a private mode. Reject them
        // rather than applying an ACL translation or writing into an inherited ACL.
        let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
        if !acl.is_null() {
            unsafe { acl_free(acl) };
            return Err(invalid(
                path,
                "extended ACL is unsupported for lock publication",
            ));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(error)
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn check_stage_policy(file: &File, path: &Path) -> io::Result<()> {
        let attribute = b"system.posix_acl_access\0";
        let length = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                attribute.as_ptr().cast(),
                std::ptr::null_mut(),
                0,
            )
        };
        if length >= 0 {
            return Err(invalid(
                path,
                "extended ACL is unsupported for lock publication",
            ));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENODATA | libc::EOPNOTSUPP) => Ok(()),
            _ => Err(error),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn check_stage_policy(_file: &File, _path: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub(super) fn publish(stage: &Path, path: &Path, replace: bool) -> io::Result<()> {
        if replace {
            return crate::replace_lock(path, stage);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use rustix::fs::{renameat_with, RenameFlags, CWD};
            renameat_with(CWD, stage, CWD, path, RenameFlags::NOREPLACE)?;
            Ok(())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("herdr-lock-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        dir.join("plugins.lock")
    }

    fn seed(path: &Path, contents: &[u8], mode: u32) {
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn snapshot_read_returns_busy_while_a_publisher_holds_the_guard() {
        let active = scratch("reader-busy");
        assert!(write_lock_contents(&active, "one\n").unwrap());
        let guard = platform::guard(parent(&active)).unwrap();
        assert_eq!(
            read_text(&active, false).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(guard);
        assert_eq!(read_text(&active, false).unwrap(), "one\n");
        fs::remove_dir_all(parent(&active)).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stage_with_inherited_acl_is_rejected_before_lock_publication() {
        let active = scratch("inherited-acl");
        let directory = active.parent().unwrap();
        assert!(std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read,file_inherit"])
            .arg(directory)
            .status()
            .unwrap()
            .success());
        let error = write_lock_contents(&active, "private\n").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(fs::read_dir(directory).unwrap().next().is_none());
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn rotation_uses_source_modes_and_keeps_replacement_mode() {
        let active = scratch("source-mode");
        let first = lock_history_beside(&active, 1);
        let second = lock_history_beside(&active, 2);
        let third = lock_history_beside(&active, 3);
        assert!(write_lock_contents(&active, "first create\n").unwrap());
        assert_eq!(mode(&active), 0o600);
        seed(&active, b"active", 0o400);
        seed(&first, b"first", 0o640);
        seed(&second, b"second", 0o644);
        seed(&third, b"third", 0o660);

        assert!(write_lock_contents(&active, "next\n").unwrap());
        for (path, bytes, expected_mode) in [
            (&active, &b"next\n"[..], 0o400),
            (&first, &b"active"[..], 0o400),
            (&second, &b"first"[..], 0o640),
            (&third, &b"second"[..], 0o644),
        ] {
            assert_eq!(fs::read(path).unwrap(), bytes);
            assert_eq!(mode(path), expected_mode);
        }
        fs::remove_dir_all(parent(&active)).unwrap();
    }

    #[test]
    fn special_mode_on_rollback_only_source_blocks_all_publication() {
        let active = scratch("special-mode");
        let leaves = [
            (active.clone(), &b"active"[..], 0o600),
            (lock_history_beside(&active, 1), &b"first"[..], 0o640),
            (lock_history_beside(&active, 2), &b"second"[..], 0o644),
            // The oldest generation is staged only for rollback.
            (lock_history_beside(&active, 3), &b"third"[..], 0o4600),
        ];
        for (path, bytes, expected_mode) in &leaves {
            seed(path, bytes, *expected_mode);
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                *expected_mode
            );
        }
        let error = write_lock_contents(&active, "next\n").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("special mode bits"));
        for (path, bytes, expected_mode) in &leaves {
            assert_eq!(fs::read(path).unwrap().as_slice(), *bytes);
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                *expected_mode
            );
        }
        assert_eq!(fs::read_dir(parent(&active)).unwrap().count(), leaves.len());
        fs::remove_dir_all(parent(&active)).unwrap();
    }

    #[test]
    fn invalid_history_is_rejected_even_when_the_active_bytes_match() {
        let active = scratch("linked-noop");
        let first = lock_history_beside(&active, 1);
        seed(&active, b"active", 0o600);

        symlink("missing", &first).unwrap();
        assert!(write_lock_contents(&active, "active").is_err());
        assert_eq!(fs::read_link(&first).unwrap(), Path::new("missing"));
        fs::remove_file(&first).unwrap();

        fs::create_dir(&first).unwrap();
        assert!(write_lock_contents(&active, "active").is_err());
        assert!(first.is_dir());
        assert_eq!(fs::read(&active).unwrap(), b"active");
        fs::remove_dir_all(parent(&active)).unwrap();
    }

    #[test]
    fn first_publication_primitive_does_not_overwrite_an_existing_name() {
        let active = scratch("no-clobber");
        let stage = Stage::new(&active, b"proposal", None).unwrap();
        seed(&active, b"somebody else's lock", 0o600);
        // Exercise the primitive itself, independently of the earlier revalidation.
        assert!(platform::publish(&stage.path, &active, false).is_err());
        assert_eq!(fs::read(&active).unwrap(), b"somebody else's lock");
        drop(stage);
        fs::remove_dir_all(parent(&active)).unwrap();
    }

    #[test]
    fn failed_active_publication_rolls_back_only_applied_history_with_original_mode() {
        let active = scratch("rollback");
        let first = lock_history_beside(&active, 1);
        let second = lock_history_beside(&active, 2);
        seed(&active, b"old active", 0o644);
        seed(&first, b"old first", 0o400);
        let _guard = platform::guard(parent(&active)).unwrap();
        let current = Leaf::open(&active).unwrap().unwrap();
        let old = Leaf::open(&first).unwrap().unwrap();
        let new = Stage::new(&active, b"new active", Some(&current)).unwrap();
        let changes = [
            Change {
                path: second.clone(),
                old: None,
                forward: Some(Stage::new(&second, &old.read(&first).unwrap(), Some(&old)).unwrap()),
                rollback: None,
            },
            Change {
                path: first.clone(),
                old: Some(old.id),
                forward: Some(
                    Stage::new(&first, &current.read(&active).unwrap(), Some(&current)).unwrap(),
                ),
                rollback: Some(Stage::new(&first, b"old first", Some(&old)).unwrap()),
            },
        ];
        // The source for the forward first generation is public, but its populated stage is
        // still private. Creation uses 0600, not chmod after writing.
        assert_eq!(mode(&changes[1].forward.as_ref().unwrap().path) & 0o077, 0);
        // Simulate a non-cooperating replacement after preflight; it must not be overwritten.
        fs::remove_file(&active).unwrap();
        seed(&active, b"another active", 0o600);
        assert!(commit(&changes, &active, &new, Some(current.id)).is_err());
        assert_eq!(fs::read(&active).unwrap(), b"another active");
        assert_eq!(fs::read(&first).unwrap(), b"old first");
        assert_eq!(mode(&first), 0o400);
        assert!(!second.exists());
        drop(changes);
        drop(new);
        fs::remove_dir_all(parent(&active)).unwrap();
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::mem::{size_of, zeroed};
    use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Security::{
        GetKernelObjectSecurity, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        IsValidSecurityDescriptor, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx, GetFileType,
        LockFileEx, MoveFileExW, UnlockFileEx, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_TYPE_DISK, LOCKFILE_EXCLUSIVE_LOCK, MOVEFILE_REPLACE_EXISTING,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    pub(super) type Identity = (u64, [u8; 16]);

    pub(super) struct Guard(File);

    impl Drop for Guard {
        fn drop(&mut self) {
            let mut offset = unsafe { zeroed::<OVERLAPPED>() };
            unsafe { UnlockFileEx(self.0.as_raw_handle(), 0, 1, 0, &mut offset) };
        }
    }

    pub(super) fn guard(parent: &Path) -> io::Result<Guard> {
        open_guard(parent, true)?.ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    pub(super) fn read_guarded<T>(
        parent: &Path,
        read: impl Fn() -> io::Result<T>,
    ) -> io::Result<T> {
        if let Some(_guard) = open_guard(parent, false)? {
            return read();
        }
        // The anchor is never removed by a cooperating writer. If one appeared
        // during our read, discard that observation and read under its shared lock.
        let result = read();
        if let Some(_guard) = open_guard(parent, false)? {
            read()
        } else {
            result
        }
    }

    fn open_guard(parent: &Path, create: bool) -> io::Result<Option<Guard>> {
        // Never unlink this stable lock anchor. Disallow delete sharing while held/open.
        let path = parent.join(".herdr-lazy-lock-guard");
        let file = match OpenOptions::new()
            .read(true)
            .write(create)
            .create(create)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if !create && error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        identity(&file, &path)?;
        let mut offset = unsafe { zeroed::<OVERLAPPED>() };
        let flags = if create {
            LOCKFILE_EXCLUSIVE_LOCK
        } else {
            windows_sys::Win32::Storage::FileSystem::LOCKFILE_FAIL_IMMEDIATELY
        };
        if unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut offset) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let guard = Guard(file);
        identity(&guard.0, &path)?;
        Ok(Some(guard))
    }

    pub(super) fn identity(file: &File, path: &Path) -> io::Result<Identity> {
        let handle = file.as_raw_handle();
        let bad = || invalid(path, "not an identifiable independent regular file");
        if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
            return Err(bad());
        }
        let mut info = unsafe { zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
            || info.nNumberOfLinks != 1
        {
            return Err(bad());
        }
        let mut id = unsafe { zeroed::<FILE_ID_INFO>() };
        if unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&mut id as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if id.FileId.Identifier == [0; 16] {
            return Err(bad());
        }
        Ok((id.VolumeSerialNumber, id.FileId.Identifier))
    }

    pub(super) fn path_identity(path: &Path) -> io::Result<Option<Identity>> {
        Ok(open(path)?.map(|leaf| leaf.id))
    }

    pub(super) fn open(path: &Path) -> io::Result<Option<Leaf>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Do not interpret an entry that the no-follow open cannot identify as absent.
                return match fs::symlink_metadata(path) {
                    Ok(_) => Err(invalid(
                        path,
                        "entry could not be opened without following it",
                    )),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error),
                };
            }
            Err(error) => return Err(error),
        };
        let id = identity(&file, path)?;
        Ok(Some(Leaf { file, id }))
    }

    pub(super) fn stage_options(options: &mut OpenOptions) {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    fn descriptor(file: &File, path: &Path) -> io::Result<(u32, Vec<u32>)> {
        // A DWORD-aligned self-relative descriptor; the ACL size is a 16-bit field.
        // Exact comparison is conservative: even semantically equivalent encodings can fail.
        let mut words = vec![0u32; (65536 + 1024) / 4];
        let sd = words.as_mut_ptr().cast();
        let mut len = 0;
        if unsafe {
            GetKernelObjectSecurity(
                file.as_raw_handle(),
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                sd,
                (words.len() * 4) as u32,
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let (mut owner, mut acl) = (std::ptr::null_mut(), std::ptr::null_mut());
        let (mut defaulted, mut present) = (0, 0);
        if len == 0
            || len as usize > words.len() * 4
            || unsafe { IsValidSecurityDescriptor(sd) } == 0
            || unsafe { GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted) } == 0
            || unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted) } == 0
            || owner.is_null()
            || present == 0
            || acl.is_null()
        {
            return Err(invalid(path, "owner or DACL unavailable"));
        }
        Ok((len, words))
    }

    pub(super) fn check_stage_policy(
        file: &File,
        source: Option<&Leaf>,
        path: &Path,
    ) -> io::Result<()> {
        let stage = descriptor(file, path)?;
        if let Some(source) = source {
            if descriptor(&source.file, path)? != stage {
                return Err(invalid(
                    path,
                    "stage owner or DACL differs from the source policy",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn publish(stage: &Path, path: &Path, replace: bool) -> io::Result<()> {
        // No COPY_ALLOWED: the stage stays on this volume with its own security descriptor.
        // ReplaceFileW merges destination metadata and is deliberately not used here.
        let wide = |path: &Path| {
            path.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let flags = if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
        if unsafe { MoveFileExW(wide(stage).as_ptr(), wide(path).as_ptr(), flags) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::windows::fs::symlink_file;

        fn scratch(name: &str) -> PathBuf {
            let directory = std::env::temp_dir()
                .join(format!("herdr-lock-windows-{name}-{}", std::process::id(),));
            fs::create_dir(&directory).unwrap();
            directory.join("plugins.lock")
        }

        fn security(path: &Path) -> (u32, Vec<u32>) {
            descriptor(&open(path).unwrap().unwrap().file, path).unwrap()
        }

        #[test]
        fn default_owner_and_dacl_survive_publication_and_mismatch_blocks_rotation() {
            let lock = scratch("dacl");
            let dir = lock.parent().unwrap();
            let first = lock_history_beside(&lock, 1);
            assert_eq!(
                read_text(&lock, false).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
            assert!(fs::read_dir(dir).unwrap().next().is_none());

            assert!(write_lock_contents(&lock, "one\n").unwrap());
            let policy = security(&lock);
            assert!(write_lock_contents(&lock, "two\n").unwrap());
            assert_eq!(policy, security(&first));
            assert_eq!(policy, security(&lock));

            assert!(std::process::Command::new("icacls.exe")
                .arg(&lock)
                .arg("/inheritance:d")
                .status()
                .unwrap()
                .success());
            let different = security(&lock);
            assert_ne!(policy, different);
            assert_eq!(
                write_lock_contents(&lock, "three\n").unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(fs::read(&lock).unwrap(), b"two\n");
            assert_eq!(fs::read(&first).unwrap(), b"one\n");
            assert_eq!(different, security(&lock));
            assert_eq!(policy, security(&first));
            fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn reparse_leaves_are_rejected_for_both_first_publication_and_restore() {
            for active_links in [true, false] {
                let lock = scratch(if active_links {
                    "active-link"
                } else {
                    "history-link"
                });
                let first = lock_history_beside(&lock, 1);
                let (source, link) = if active_links {
                    (&first, &lock)
                } else {
                    (&lock, &first)
                };
                symlink_file(source, link).unwrap();
                assert_eq!(
                    write_lock_contents(&lock, "new\n").unwrap_err().kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(
                    read_text(&lock, true).unwrap_err().kind(),
                    io::ErrorKind::InvalidData
                );
                assert!(fs::symlink_metadata(source).is_err());
                assert_eq!(fs::read_link(link).unwrap(), *source);
                fs::remove_dir_all(lock.parent().unwrap()).unwrap();
            }
        }
    }
}
