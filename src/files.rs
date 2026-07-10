#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/files.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2015-2022, 2025 Benno Schulenberg

use crate::definitions::*;
use crate::global::{state, state_mut, with_state, with_state_mut};
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::{ISSET, SET, UNSET, TOGGLE};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(not(feature = "tiny"))]
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
#[cfg(feature = "operatingdir")]
use std::cell::RefCell;

// Re-export stubs for winio/text/search/nano functions referenced here.
// These will be replaced by real implementations when those modules are ported.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const LOCKSIZE: usize = 1024;

// The operating directory is a security boundary, not just a pathname
// predicate.  Keep the directory open and resolve every user-controlled path
// through that handle.  cap-std implements beneath/no-escape resolution on
// Linux, macOS, FreeBSD, and Windows, including safe fallback walking when the
// newest native primitive is unavailable.
#[cfg(feature = "operatingdir")]
struct OperatingRoot {
    display_path: PathBuf,
    dir: cap_std::fs::Dir,
}

#[cfg(feature = "operatingdir")]
thread_local! {
    static OPERATING_ROOT: RefCell<Option<OperatingRoot>> = const { RefCell::new(None) };
}

#[cfg(feature = "operatingdir")]
fn relative_to_root(root: &OperatingRoot, path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        path.strip_prefix(&root.display_path)
            .map(|relative| {
                if relative.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    relative.to_path_buf()
                }
            })
            .map_err(|_| io::Error::new(
                io::ErrorKind::PermissionDenied,
                "path is outside the operating directory",
            ))
    } else {
        Ok(path.to_path_buf())
    }
}

#[cfg(feature = "operatingdir")]
fn with_operating_root<T>(
    path: &Path,
    operation: impl FnOnce(&OperatingRoot, &Path) -> io::Result<T>,
) -> Option<io::Result<T>> {
    OPERATING_ROOT.with(|slot| {
        let root = slot.borrow();
        root.as_ref().map(|root| {
            let relative = relative_to_root(root, path)?;
            operation(root, &relative)
        })
    })
}

fn operating_root_required_but_unavailable() -> bool {
    #[cfg(feature = "operatingdir")]
    {
        state().operating_dir.is_some()
            && OPERATING_ROOT.with(|slot| slot.borrow().is_none())
    }
    #[cfg(not(feature = "operatingdir"))]
    {
        false
    }
}

fn operating_root_is_active() -> bool {
    #[cfg(feature = "operatingdir")]
    {
        OPERATING_ROOT.with(|slot| slot.borrow().is_some())
    }
    #[cfg(not(feature = "operatingdir"))]
    {
        false
    }
}

#[derive(Clone, Copy, Default)]
struct PathOpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
    nofollow: bool,
    mode: u32,
}

fn open_path_with(path: &Path, options: PathOpenOptions) -> io::Result<File> {
    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(path, |root, relative| {
        let mut cap_options = cap_std::fs::OpenOptions::new();
        cap_options
            .read(options.read)
            .write(options.write)
            .append(options.append)
            .truncate(options.truncate)
            .create(options.create)
            .create_new(options.create_new);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            cap_options.mode(options.mode);
            if options.nofollow {
                cap_options.custom_flags(libc::O_NOFOLLOW);
            }
        }
        root.dir
            .open_with(relative, &cap_options)
            .map(cap_std::fs::File::into_std)
    }) {
        return result;
    }

    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized",
        ));
    }

    let mut ambient_options = OpenOptions::new();
    ambient_options
        .read(options.read)
        .write(options.write)
        .append(options.append)
        .truncate(options.truncate)
        .create(options.create)
        .create_new(options.create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        ambient_options.mode(options.mode);
        if options.nofollow {
            ambient_options.custom_flags(libc::O_NOFOLLOW);
        }
    }
    ambient_options.open(path)
}

fn open_path(path: &Path) -> io::Result<File> {
    open_path_with(path, PathOpenOptions {
        read: true,
        ..PathOpenOptions::default()
    })
}

fn path_exists_nofollow(path: &Path) -> io::Result<bool> {
    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(path, |root, relative| {
        match root.dir.symlink_metadata(relative) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }) {
        return result;
    }
    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized"));
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_path(path: &Path) -> io::Result<()> {
    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(path, |root, relative| {
        root.dir.remove_file(relative)
    }) {
        return result;
    }
    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized"));
    }
    std::fs::remove_file(path)
}

fn rename_path(from: &Path, to: &Path, source: &File) -> io::Result<()> {
    let _ = source;
    #[cfg(feature = "operatingdir")]
    {
        let result = OPERATING_ROOT.with(|slot| {
            let root = slot.borrow();
            root.as_ref().map(|root| {
                let _from = relative_to_root(root, from)?;
                let to = relative_to_root(root, to)?;
                #[cfg(windows)]
                {
                    replace_capability_file_on_windows(root, source, &to)
                }
                #[cfg(not(windows))]
                {
                    let _ = source;
                    root.dir.rename(_from, &root.dir, to)
                }
            })
        });
        if let Some(result) = result {
            return result;
        }
    }
    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized"));
    }
    std::fs::rename(from, to)
}

/// Atomically install an already-open staging file on Windows.
///
/// `SetFileInformationByHandle(FileRenameInfo)` rejects a non-NULL
/// `RootDirectory` with ERROR_INVALID_PARAMETER (that field is honored only
/// by `NtSetInformationFile`), so a handle-relative rename is not available
/// through the Win32 API.  Instead, both the source and the destination
/// parent are resolved to their final pathnames *from already-open
/// capability-confined handles*, and the rename uses those resolved names
/// (`std::fs::rename` replaces existing files on Windows via
/// MOVEFILE_REPLACE_EXISTING).  A directory swapped after resolution can at
/// worst fail the rename; the names cannot come from re-walking an ambient,
/// attacker-substitutable path.
#[cfg(all(windows, feature = "operatingdir"))]
fn replace_capability_file_on_windows(
    root: &OperatingRoot,
    source: &File,
    destination: &Path,
) -> io::Result<()> {
    let basename = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "replacement destination has no file name",
        )
    })?;
    let parent = destination.parent().filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = root.dir.open_dir(parent)?;

    let source_path = final_path_from_handle(source)?;
    let parent_path = final_path_from_handle(&parent.into_std_file())?;
    std::fs::rename(source_path, parent_path.join(basename))
}

/// Resolve an open handle to its final `\\?\`-prefixed pathname.
#[cfg(all(windows, feature = "operatingdir"))]
fn final_path_from_handle(file: &File) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED,
    };

    let mut buffer = vec![0u16; 512];
    loop {
        let length = unsafe {
            GetFinalPathNameByHandleW(
                HANDLE(file.as_raw_handle()),
                &mut buffer,
                FILE_NAME_NORMALIZED,
            )
        } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length <= buffer.len() {
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length])));
        }
        buffer.resize(length, 0);
    }
}

fn sync_parent_of(path: &Path) -> io::Result<()> {
    let parent = usable_parent(path);
    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(parent, |root, relative| {
        root.dir.open_dir(relative)?.into_std_file().sync_all()
    }) {
        return result;
    }
    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized"));
    }
    File::open(parent)?.sync_all()
}

/// Return whether the actual resolved object is a directory without allowing
/// resolution to leave the retained operating-directory capability.
pub(crate) fn confined_is_dir(path: impl AsRef<Path>) -> io::Result<bool> {
    let path = path.as_ref();
    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(path, |root, relative| {
        root.dir.metadata(relative).map(|metadata| metadata.is_dir())
    }) {
        return result;
    }
    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized"));
    }
    std::fs::metadata(path).map(|metadata| metadata.is_dir())
}

#[derive(Clone, Copy)]
struct PathInfo {
    is_dir: bool,
    is_special: bool,
    is_fifo: bool,
    mode: u32,
}

fn path_info(path: &Path) -> io::Result<PathInfo> {
    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(path, |root, relative| {
        let metadata = root.dir.metadata(relative)?;
        #[cfg(unix)]
        {
            use cap_std::fs::{FileTypeExt as _, MetadataExt as _};
            let file_type = metadata.file_type();
            return Ok(PathInfo {
                is_dir: metadata.is_dir(),
                is_special: file_type.is_char_device() || file_type.is_block_device(),
                is_fifo: file_type.is_fifo(),
                mode: metadata.mode(),
            });
        }
        #[cfg(not(unix))]
        {
            return Ok(PathInfo {
                is_dir: metadata.is_dir(),
                is_special: false,
                is_fifo: false,
                mode: 0,
            });
        }
    }) {
        return result;
    }
    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized"));
    }

    let metadata = std::fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
        let file_type = metadata.file_type();
        Ok(PathInfo {
            is_dir: metadata.is_dir(),
            is_special: file_type.is_char_device() || file_type.is_block_device(),
            is_fifo: file_type.is_fifo(),
            mode: metadata.permissions().mode(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(PathInfo {
            is_dir: metadata.is_dir(),
            is_special: false,
            is_fifo: false,
            mode: 0,
        })
    }
}

/// Read directory entry names through the retained capability.  Names alone
/// are returned so callers cannot accidentally regain an ambient path handle.
pub(crate) fn confined_read_dir(path: impl AsRef<Path>) -> io::Result<Vec<std::ffi::OsString>> {
    let path = path.as_ref();
    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(path, |root, relative| {
        root.dir
            .read_dir(relative)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    }) {
        return result;
    }
    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized"));
    }
    std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

// ---------------------------------------------------------------------------
// Stub calls to other modules (forward declarations)
// ---------------------------------------------------------------------------

// ---- Delegating wrappers: call the real implementations ----

#[inline] fn statusline(level: MessageType, msg: &str) { crate::winio::statusline(level, msg); }
#[inline] fn statusbar(msg: &str) { crate::winio::statusbar(msg); }
#[inline] fn titlebar(extra: Option<&str>) { crate::winio::titlebar(extra); }
fn blank_bottombars() { crate::winio::blank_bottombars(); }
fn wipe_statusbar() { crate::winio::wipe_statusbar(); }
/// C: beep() — ncurses; ring the terminal bell.
#[inline]
fn beep() { crate::winio::beep() }
/// C: napms(ms) — ncurses; lets flash messages linger to be read.
#[inline]
fn napms(ms: i32) { crate::winio::napms(ms.max(0) as u64) }

// Helper: check if file is a special file (char device, block device, or socket)
fn is_special_file(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        // C's is_dev_chr/is_dev_blk check only matches S_ISCHR || S_ISBLK; sockets
        // are NOT treated as "device files" (mirrors files.c).
        meta.file_type().is_char_device() || meta.file_type().is_block_device()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

// Helper: check if file is a FIFO
fn is_fifo_file(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_fifo()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn usable_parent(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

static STAGING_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

struct CapabilityTempFile {
    relative_path: PathBuf,
    file: Option<File>,
    installed: bool,
}

impl CapabilityTempFile {
    fn as_file(&self) -> &File {
        self.file.as_ref().expect("capability temp file is open")
    }

    fn as_file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("capability temp file is open")
    }

    fn persist(mut self, destination: &Path) -> io::Result<File> {
        rename_path(&self.relative_path, destination, self.as_file())?;
        self.installed = true;
        Ok(self.file.take().expect("capability temp file is open"))
    }
}

impl Drop for CapabilityTempFile {
    fn drop(&mut self) {
        if self.installed {
            return;
        }
        #[cfg(feature = "operatingdir")]
        OPERATING_ROOT.with(|slot| {
            if let Some(root) = slot.borrow().as_ref() {
                let _ = root.dir.remove_file(&self.relative_path);
            }
        });
    }
}

enum StagingFile {
    Ambient(tempfile::NamedTempFile),
    Capability(CapabilityTempFile),
}

impl StagingFile {
    fn as_file(&self) -> &File {
        match self {
            Self::Ambient(file) => file.as_file(),
            Self::Capability(file) => file.as_file(),
        }
    }

    fn as_file_mut(&mut self) -> &mut File {
        match self {
            Self::Ambient(file) => file.as_file_mut(),
            Self::Capability(file) => file.as_file_mut(),
        }
    }

    fn persist(self, destination: &Path) -> io::Result<File> {
        match self {
            Self::Ambient(file) => file
                .persist(destination)
                .map_err(|error| error.error),
            Self::Capability(file) => file.persist(destination),
        }
    }
}

fn create_staging_file(parent: &Path, prefix: &str) -> io::Result<StagingFile> {
    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(parent, |root, relative_parent| {
        use std::sync::atomic::Ordering;

        for _ in 0..128 {
            let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = relative_parent.join(format!(
                "{}{}.{:016x}",
                prefix,
                std::process::id(),
                sequence,
            ));
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(windows)]
            {
                use cap_std::fs::OpenOptionsExt;
                use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
                use windows::Win32::Storage::FileSystem::DELETE;

                options.access_mode(GENERIC_READ.0 | GENERIC_WRITE.0 | DELETE.0);
            }
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            match root.dir.open_with(&candidate, &options) {
                Ok(file) => {
                    return Ok(StagingFile::Capability(CapabilityTempFile {
                        relative_path: candidate,
                        file: Some(file.into_std()),
                        installed: false,
                    }));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique staging file",
        ))
    }) {
        return result;
    }

    if operating_root_required_but_unavailable() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "operating-directory capability is not initialized"));
    }
    tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(parent)
        .map(StagingFile::Ambient)
}

fn last_path_separator(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find(|(_, ch)| *ch == '/' || *ch == '\\')
        .map(|(idx, _)| idx)
}

fn path_join_display(base: &str, child: &str) -> String {
    Path::new(base).join(child).to_string_lossy().into_owned()
}

#[inline]
fn printable_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Expand a leading `~` without converting the path through UTF-8.  Only the
/// returned `PathBuf` is authoritative; callers may derive a lossy string for
/// UI text with `printable_path()`.
pub fn expand_leading_tilde_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut components = path.components();
    let first = match components.next() {
        Some(Component::Normal(component)) => component,
        _ => return path.to_path_buf(),
    };

    #[cfg(unix)]
    let first_bytes = {
        use std::os::unix::ffi::OsStrExt;
        first.as_bytes()
    };
    #[cfg(not(unix))]
    let first_bytes = first.to_str().unwrap_or("").as_bytes();

    if first_bytes.first() != Some(&b'~') {
        return path.to_path_buf();
    }

    let home = if first_bytes.len() == 1 {
        crate::utils::get_homedir();
        state().homedir_raw.clone()
    } else {
        #[cfg(unix)]
        {
            let username = match std::ffi::CString::new(&first_bytes[1..]) {
                Ok(username) => username,
                Err(_) => return path.to_path_buf(),
            };
            let entry = unsafe { libc::getpwnam(username.as_ptr()) };
            if entry.is_null() {
                None
            } else {
                use std::os::unix::ffi::OsStrExt;
                let bytes = unsafe { std::ffi::CStr::from_ptr((*entry).pw_dir) }.to_bytes();
                Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
            }
        }
        #[cfg(not(unix))]
        {
            None
        }
    };

    match home {
        Some(home) => home.join(components.as_path()),
        None => path.to_path_buf(),
    }
}

fn set_openfile_filename(openfile: &mut OpenFileStruct, path: PathBuf) {
    openfile.filename = printable_path(&path);
    openfile.filename_path = path;
}

fn openfile_filename_path(openfile: &OpenFileStruct) -> Option<&Path> {
    if openfile.filename_path.as_os_str().is_empty() {
        None
    } else {
        Some(&openfile.filename_path)
    }
}

/// C: make_new_node(prev) — nano.c.
#[inline]
fn make_new_node(prev: Option<LinePtr>) -> LinePtr {
    crate::nano::make_new_node(prev.as_ref())
}

#[inline] fn ingraft_buffer(topline: LinePtr) { crate::cut::ingraft_buffer(topline); }
#[inline] fn xplustabs() -> usize { crate::utils::xplustabs() }

#[cfg(feature = "color")]
fn precalc_multicolorinfo() { crate::color::precalc_multicolorinfo(); }
#[cfg(not(feature = "color"))]
fn precalc_multicolorinfo() {}

#[cfg(feature = "color")]
fn find_and_prime_applicable_syntax() { crate::color::find_and_prime_applicable_syntax(); }
#[cfg(not(feature = "color"))]
fn find_and_prime_applicable_syntax() {}

// Signature adapter: call site passes isize, real fn takes usize.
fn less_than_a_screenful(was_lineno: isize, was_leftedge: usize) -> bool {
    crate::winio::less_than_a_screenful(was_lineno as usize, was_leftedge)
}

// Signature adapter: call site passes Option<&LinePtr>, real fn takes &str.
fn leftedge_for(xpt: usize, current: Option<&LinePtr>) -> usize {
    let data = current.map(|l| l.borrow().data.clone()).unwrap_or_default();
    crate::winio::leftedge_for(xpt, &data)
}

#[cfg(not(feature = "tiny"))]
fn ensure_firstcolumn_is_aligned() { crate::winio::ensure_firstcolumn_is_aligned(); }
#[cfg(feature = "tiny")]
fn ensure_firstcolumn_is_aligned() {}

#[inline] fn warn_and_briefly_pause(msg: &str) { crate::winio::warn_and_briefly_pause(msg); }
fn ask_user(yesno: bool, msg: &str) -> i32 { crate::prompt::ask_user(yesno, msg) }
fn in_restricted_mode() -> bool { crate::ISSET!(crate::definitions::RESTRICTED) }
fn breadth(s: &str) -> usize { crate::utils::breadth(s) }
fn display_string(s: &str, from: usize, room: usize, isdata: bool, isprompt: bool) -> String {
    crate::winio::display_string(s, from, room, isdata, isprompt)
}
fn mbstrcasecmp(a: &str, b: &str) -> i32 {
    let a_lc = a.to_lowercase(); let b_lc = b.to_lowercase();
    a_lc.cmp(&b_lc) as i32
}

/// C: reconnect_and_store_state() — nano.c; reattach the keyboard to stdin.
#[inline]
fn reconnect_and_store_state() { crate::nano::reconnect_and_store_state(); }
fn terminal_init() { crate::nano::terminal_init(); }
fn doupdate() {}  // stub: no crate::winio::doupdate exists
fn isendwin() -> bool { false }  // stub: no crate::winio::isendwin exists

#[inline] fn install_handler_for_Ctrl_C() { crate::nano::install_handler_for_Ctrl_C(); }
#[inline] fn restore_handler_for_Ctrl_C() { crate::nano::restore_handler_for_Ctrl_C(); }
#[inline] fn block_sigwinch(block: bool) { crate::nano::block_sigwinch(block); }
fn enable_kb_interrupt() { crate::nano::enable_kb_interrupt(); }
fn close_and_go() { crate::nano::close_and_go() }
fn finish() { crate::nano::finish() }

// Undo record delegation
#[inline] fn add_undo(utype: UndoType, msg: Option<&str>) { crate::text::add_undo(utype, msg); }
#[inline] fn update_undo(utype: UndoType) { crate::text::update_undo(utype); }
fn discard_until(target: *mut crate::definitions::UndoStruct) {
    crate::text::discard_until(target as *const crate::definitions::UndoStruct)
}
/// C: get_region(&top, &top_x, &bot, &bot_x) — frame the marked region.
fn get_region(
    topline: &mut Option<LinePtr>, top_x: &mut usize,
    botline: &mut Option<LinePtr>, bot_x: &mut usize,
) {
    let (t_ln, t_x, b_ln, b_x) = crate::utils::get_region();
    *topline = crate::utils::line_from_number(t_ln as isize);
    *botline = crate::utils::line_from_number(b_ln as isize);
    *top_x = t_x;
    *bot_x = b_x;
}

// Signature adapter: call site passes LinePtr by value, real fn takes &LinePtr.
fn delete_node(node: LinePtr) { crate::nano::delete_node(&node); }

/// C: add_or_remove_pipe_symbol_from_answer() — prompt.c.
fn add_or_remove_pipe_symbol_from_answer() {
    #[cfg(not(feature = "tiny"))]
    crate::prompt::add_or_remove_pipe_symbol_from_answer();
}

/// C: restore_cursor_position_if_any() — history.c.
fn restore_cursor_position_if_any() {
    #[cfg(feature = "histories")]
    crate::history::restore_cursor_position_if_any();
}

/// C: do_undo() — text.c.
fn do_undo() {
    #[cfg(not(feature = "tiny"))]
    crate::text::do_undo();
}

/// C: do_credits() — winio.c easter egg.
fn do_credits() {
    crate::winio::do_credits();
}
#[inline]
fn do_prompt(
    menu: u32,
    given: &str,
    history_kind: Option<crate::history::HistoryKind>,
    refresh: fn(),
    msg: &str,
    _extra: &str,
) -> i32 {
    crate::prompt::do_prompt(menu, Some(given), history_kind, Some(refresh), msg)
}
#[inline] fn edit_refresh() { crate::winio::edit_refresh(); }

/// C: browse_in(path) — browser.c; pick a file via the file browser.
fn browse_in(path: &str) -> Option<String> {
    #[cfg(feature = "browser")]
    { crate::browser::browse_in(path) }
    #[cfg(not(feature = "browser"))]
    { let _ = path; None }
}

#[inline] fn func_from_key(response: i32) -> Option<FuncPtr> { crate::global::func_from_key(response) }

/// C: update_history(&execute_history, answer, PRUNE_DUPLICATE) — history.c.
/// The only call site updates the execute-command history.
fn update_history(_history: &mut Option<LinePtr>, s: &str, prune: bool) {
    #[cfg(feature = "histories")]
    crate::history::update_history(crate::history::HistoryKind::Execute, s, prune);
    #[cfg(not(feature = "histories"))]
    { let _ = (s, prune); }
}

// Stub display helper
fn COLS() -> usize {
    state().midwin.cols as usize
}
fn LINES() -> usize {
    state().midwin.rows as usize
}

// ---------------------------------------------------------------------------
// crop_to_fit — return the given file name cropped to fit within `room` cols
// C: char *crop_to_fit(const char *name, int room)
// ---------------------------------------------------------------------------
pub fn crop_to_fit(name: &str, room: isize) -> String {
    // room is signed (C uses `int room`) so an underflowed/negative budget falls
    // into the `room < 4` "_" case instead of wrapping to a huge usize.
    if (breadth(name) as isize) <= room {
        return display_string(name, 0, room.max(0) as usize, false, false);
    }

    if room < 4 {
        return "_".to_string();
    }

    let room = room as usize;
    let mut clipped = display_string(name, breadth(name) - room + 3, room, false, false);
    clipped.insert_str(0, "...");
    clipped
}

// ---------------------------------------------------------------------------
// LOCK FILE support (!NANO_TINY)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "tiny"))]
pub const LOCKING_PREFIX: &str = ".";
#[cfg(not(feature = "tiny"))]
pub const LOCKING_SUFFIX: &str = ".swp";

#[cfg(feature = "tiny")]
pub fn delete_lockfile(_lockfilename: impl AsRef<Path>, _lockfile: Option<&File>) -> bool {
    true
}

#[cfg(not(feature = "tiny"))]
fn open_lockfile_for_create(lockfilename: &Path) -> io::Result<File> {
    // O_EXCL is the ownership boundary: another editor (or a racing symlink)
    // must never be unlinked and silently replaced.
    open_path_with(lockfilename, PathOpenOptions {
        read: true,
        write: true,
        create_new: true,
        nofollow: true,
        mode: 0o666,
        ..PathOpenOptions::default()
    })
}

#[cfg(not(feature = "tiny"))]
fn open_existing_lockfile(lockfilename: &Path) -> io::Result<File> {
    let file = open_path_with(lockfilename, PathOpenOptions {
        read: true,
        write: true,
        nofollow: true,
        ..PathOpenOptions::default()
    })?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "lock path is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if file.metadata()?.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "lock file has multiple hard links",
            ));
        }
    }
    Ok(file)
}

#[cfg(all(windows, not(feature = "tiny")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowsFileIdentity {
    volume_serial_number: u32,
    file_index: u64,
    link_count: u32,
}

#[cfg(all(windows, not(feature = "tiny")))]
fn windows_file_identity(file: &File) -> io::Result<WindowsFileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information)
            .map_err(|_| io::Error::last_os_error())?;
    }
    Ok(WindowsFileIdentity {
        volume_serial_number: information.dwVolumeSerialNumber,
        file_index: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
        link_count: information.nNumberOfLinks,
    })
}

#[cfg(not(feature = "tiny"))]
fn lockfile_still_names(file: &File, lockfilename: &Path) -> bool {
    let held = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    // Resolve the name through the same retained directory capability.  The
    // held descriptor is the only object ever mutated; this second descriptor
    // is solely an identity check, so a subsequent rename cannot redirect the
    // write to an attacker-selected object.
    let named_file = match open_path_with(lockfilename, PathOpenOptions {
        read: true,
        nofollow: true,
        ..PathOpenOptions::default()
    }) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let named = match named_file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if !held.is_file() || !named.file_type().is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        return held.dev() == named.dev() && held.ino() == named.ino() && held.nlink() == 1;
    }
    #[cfg(windows)]
    {
        let held = match windows_file_identity(file) {
            Ok(identity) => identity,
            Err(_) => return false,
        };
        let named = match windows_file_identity(&named_file) {
            Ok(identity) => identity,
            Err(_) => return false,
        };
        held == named && held.link_count == 1
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/* C: bool delete_lockfile(const char *lockfilename)
 * Delete the lock file only while the name still identifies the descriptor
 * retained by this editor.  Return TRUE on success, and FALSE otherwise. */
#[cfg(not(feature = "tiny"))]
pub fn delete_lockfile(lockfilename: impl AsRef<Path>, lockfile: Option<&File>) -> bool {
    let lockfilename = lockfilename.as_ref();
    let Some(lockfile) = lockfile else {
        statusline(MessageType::Mild,
            &format!("Refusing to delete lock file without its handle: {}", lockfilename.display()));
        return false;
    };

    if !lockfile_still_names(lockfile, lockfilename) {
        // Preserve the historical success result when the lock name is
        // already gone, but never unlink a replacement object.
        if matches!(path_exists_nofollow(lockfilename), Ok(false)) {
            return true;
        }
        statusline(MessageType::Mild,
            &format!("Refusing to delete changed lock file: {}", lockfilename.display()));
        return false;
    }

    match remove_path(lockfilename) {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            statusline(MessageType::Mild,
                &format!("Error deleting lock file {}: {}", lockfilename.display(), e));
            false
        }
    }
}

#[cfg(not(feature = "tiny"))]
fn build_lockdata(filename: &Path, modified: bool) -> Option<Vec<u8>> {
    let pid = std::process::id();

    let username: String = {
        #[cfg(unix)]
        unsafe {
            let uid = libc::geteuid();
            let pw = libc::getpwuid(uid);
            if pw.is_null() {
                statusline(MessageType::Mild, "Couldn't determine my identity for lock file");
                return None;
            }
            let name = std::ffi::CStr::from_ptr((*pw).pw_name);
            name.to_string_lossy().into_owned()
        }
        #[cfg(not(unix))]
        String::from("unknown")
    };

    #[cfg(unix)]
    let hostname: String = {
        let mut buf = [0u8; 32];
        let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, 31) };
        if ret < 0 {
            statusline(MessageType::Mild,
                &format!("Couldn't determine hostname: {}", io::Error::last_os_error()));
            return None;
        }
        buf[31] = 0;
        let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
        cstr.to_string_lossy().into_owned()
    };

    #[cfg(not(unix))]
    let hostname: String = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".to_string());

    let mut lockdata = vec![0u8; LOCKSIZE];
    lockdata[0] = 0x62;
    lockdata[1] = 0x30;

    let progname = format!("nano {}", GNU_NANO_VERSION);
    let progname_bytes = progname.as_bytes();
    let plen = progname_bytes.len().min(10);
    lockdata[2..2 + plen].copy_from_slice(&progname_bytes[..plen]);

    lockdata[24] = (pid % 256) as u8;
    lockdata[25] = ((pid / 256) % 256) as u8;
    lockdata[26] = ((pid / (256 * 256)) % 256) as u8;
    lockdata[27] = (pid / (256 * 256 * 256)) as u8;

    let uname_bytes = username.as_bytes();
    let ulen = uname_bytes.len().min(16);
    lockdata[28..28 + ulen].copy_from_slice(&uname_bytes[..ulen]);

    let hname_bytes = hostname.as_bytes();
    let hlen = hname_bytes.len().min(32);
    lockdata[68..68 + hlen].copy_from_slice(&hname_bytes[..hlen]);

    #[cfg(unix)]
    let fname_bytes = {
        use std::os::unix::ffi::OsStrExt;
        filename.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let fname_bytes = filename.to_string_lossy().as_bytes().to_vec();
    let flen = fname_bytes.len().min(768);
    lockdata[108..108 + flen].copy_from_slice(&fname_bytes[..flen]);
    lockdata[1007] = if modified { 0x55 } else { 0x00 };
    Some(lockdata)
}

/* Write lock contents through the descriptor acquired for this buffer. */
#[cfg(not(feature = "tiny"))]
pub fn write_lockfile(
    lockfile: &mut File,
    lockfilename: &Path,
    filename: &Path,
    modified: bool,
) -> bool {
    if !lockfile_still_names(lockfile, lockfilename) {
        statusline(MessageType::Mild, &format!("Lock file changed: {}", lockfilename.display()));
        return false;
    }
    let lockdata = match build_lockdata(filename, modified) {
        Some(data) => data,
        None => return false,
    };
    let result = lockfile
        .seek(SeekFrom::Start(0))
        .and_then(|_| lockfile.set_len(0))
        .and_then(|_| lockfile.write_all(&lockdata))
        .and_then(|_| lockfile.sync_all());
    if let Err(error) = result {
        statusline(MessageType::Mild,
            &format!("Error writing lock file {}: {}", lockfilename.display(), error));
        return false;
    }
    if !lockfile_still_names(lockfile, lockfilename) {
        statusline(MessageType::Mild, &format!("Lock file changed: {}", lockfilename.display()));
        return false;
    }
    true
}

#[cfg(not(feature = "tiny"))]
fn create_lockfile(lockfilename: &Path, filename: &Path, modified: bool) -> Option<File> {
    let mut lockfile = match open_lockfile_for_create(lockfilename) {
        Ok(file) => file,
        Err(error) => {
            statusline(MessageType::Mild,
                &format!("Error writing lock file {}: {}", lockfilename.display(), error));
            return None;
        }
    };
    if write_lockfile(&mut lockfile, lockfilename, filename, modified) {
        Some(lockfile)
    } else {
        let _ = delete_lockfile(lockfilename, Some(&lockfile));
        drop(lockfile);
        None
    }
}

/// Sentinel indicating user chose not to open a locked file.
#[cfg(not(feature = "tiny"))]
pub const SKIPTHISFILE: i32 = -2;

/* C: char *do_lockfile(const char *filename, bool ask_the_user)
 * First check if a lock file already exists.  If so, and ask_the_user is TRUE,
 * ask whether to open the corresponding file anyway.  Return SKIPTHISFILE when
 * the user answers "No", return the lock filename on success, and return None on
 * failure.  Rust retains the exclusively acquired descriptor alongside the
 * pathname, and a special Err(()) means SKIPTHISFILE. */
#[cfg(not(feature = "tiny"))]
pub fn do_lockfile(filename: &Path, ask_the_user: bool) -> Result<Option<(PathBuf, File)>, ()> {
    // Build lock filename: <dir>/.<basename>.swp — byte-preserving, so a
    // non-UTF-8 file name locks its exact sibling name.
    let dirname = usable_parent(filename);
    let mut lockname = std::ffi::OsString::from(LOCKING_PREFIX);
    lockname.push(filename.file_name().unwrap_or_default());
    lockname.push(LOCKING_SUFFIX);
    let lockfilename: PathBuf = dirname.join(&lockname);
    let lockfilename_str = printable_path(&lockfilename);

    // symlink_metadata also sees dangling symlinks.  Such a path must count as
    // occupied so create_new() can fail closed instead of following it.
    let lock_exists = match path_exists_nofollow(&lockfilename) {
        Ok(exists) => exists,
        Err(error) => {
            statusline(MessageType::Alert,
                &format!("Error checking lock file {}: {}", lockfilename_str, error));
            return Ok(None);
        }
    };
    if lock_exists && !ask_the_user {
        blank_bottombars();
        statusline(MessageType::Alert, "Someone else is also editing this file");
        napms(1200);
        return Ok(None);
    } else if lock_exists {
        // Read and parse the lock file
        match open_existing_lockfile(&lockfilename) {
            Err(e) => {
                statusline(MessageType::Alert,
                    &format!("Error opening lock file {}: {}", lockfilename_str, e));
                return Ok(None);
            }
            Ok(mut f) => {
                let mut lockbuf = Vec::with_capacity(LOCKSIZE);
                let readamt = match (&mut f)
                    .take((LOCKSIZE + 1) as u64)
                    .read_to_end(&mut lockbuf)
                {
                    Ok(amount) => amount,
                    Err(e) => {
                        statusline(MessageType::Alert,
                            &format!("Error reading lock file {}: {}", lockfilename_str, e));
                        return Ok(None);
                    }
                };

                // Validate magic bytes and minimum size
                if readamt < 68 || lockbuf[0] != 0x62 || lockbuf[1] != 0x30 {
                    statusline(MessageType::Alert,
                        &format!("Bad lock file is ignored: {}", lockfilename_str));
                    return Ok(None);
                }

                // Extract program name (bytes 2-11)
                let lockprog: String = {
                    let bytes = &lockbuf[2..12];
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(10);
                    String::from_utf8_lossy(&bytes[..end]).into_owned()
                };

                // Extract PID (bytes 24-27, little endian)
                let lockpid = ((lockbuf[27] as u32) * 256 * 256 * 256)
                    + ((lockbuf[26] as u32) * 256 * 256)
                    + ((lockbuf[25] as u32) * 256)
                    + (lockbuf[24] as u32);

                // Extract username (bytes 28-43)
                let lockuser: String = {
                    let bytes = &lockbuf[28..44];
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(16);
                    String::from_utf8_lossy(&bytes[..end]).into_owned()
                };

                let pidstring = format!("{}", lockpid);

                // Display newlines in filenames as ^J
                state_mut().as_an_at = false;

                let question = "File %s is being edited by %s (with %s, PID %s); open anyway?";
                let total_fixed = breadth(question)
                    + breadth(&lockuser)
                    + breadth(&lockprog)
                    + breadth(&pidstring);
                let room = COLS() as isize - total_fixed as isize + 7;
                let postedname = crop_to_fit(&printable_path(filename), room);
                let promptstr = question
                    .replacen("%s", &postedname, 1)
                    .replacen("%s", &lockuser, 1)
                    .replacen("%s", &lockprog, 1)
                    .replacen("%s", &pidstring, 1);

                let choice = ask_user(YESORNO, &promptstr);

                // When the user cancelled while we're still starting up, quit.
                let we_are_running = state().we_are_running;
                if choice == CANCEL && !we_are_running {
                    finish();
                }

                if choice != YES {
                    wipe_statusbar();
                    return Err(());
                }

                // Override the stale lock through the exact descriptor that we
                // inspected.  A swapped directory entry is neither unlinked nor
                // overwritten and causes the identity checks to fail closed.
                if write_lockfile(&mut f, &lockfilename, filename, false) {
                    return Ok(Some((lockfilename, f)));
                }
                return Ok(None);
            }
        }
    }

    Ok(create_lockfile(&lockfilename, filename, false)
        .map(|lockfile| (lockfilename, lockfile)))
}

/* C: void stat_with_alloc(const char *filename, struct stat **pstat)
 * Perform a stat call on the given filename.  On success, *pstat points to
 * the stat's result.  On failure, *pstat is freed and made NULL. */
#[cfg(not(feature = "tiny"))]
pub fn stat_with_alloc(filename: impl AsRef<Path>) -> Option<FileStat> {
    let filename = filename.as_ref();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        #[cfg(feature = "operatingdir")]
        if let Some(result) = with_operating_root(filename, |root, relative| {
            root.dir.metadata(relative)
        }) {
            use cap_std::fs::MetadataExt as _;
            return result.ok().map(|meta| FileStat {
                st_mtime: meta.mtime(),
                st_dev: meta.dev(),
                st_ino: meta.ino(),
                st_uid: meta.uid(),
                st_gid: meta.gid(),
                st_mode: meta.mode(),
                st_atime: meta.atime(),
                st_atime_nsec: meta.atime_nsec() as i64,
                st_mtime_nsec: meta.mtime_nsec() as i64,
            });
        }
        if operating_root_required_but_unavailable() {
            return None;
        }
        match std::fs::metadata(filename) {
            Ok(meta) => {
                Some(FileStat {
                    st_mtime: meta.mtime(),
                    st_dev: meta.dev(),
                    st_ino: meta.ino(),
                    st_uid: meta.uid(),
                    st_gid: meta.gid(),
                    st_mode: meta.mode(),
                    st_atime: meta.atime(),
                    st_atime_nsec: meta.atime_nsec() as i64,
                    st_mtime_nsec: meta.mtime_nsec() as i64,
                })
            }
            Err(_) => None,
        }
    }
    #[cfg(not(unix))]
    {
        #[cfg(feature = "operatingdir")]
        if let Some(result) = with_operating_root(filename, |root, relative| {
            root.dir.metadata(relative)
        }) {
            return result.ok().map(|meta| FileStat {
                st_mtime: meta.modified().ok()
                    .and_then(|t| t.into_std().duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
                st_dev: 0,
                st_ino: 0,
            });
        }
        if operating_root_required_but_unavailable() {
            return None;
        }
        match std::fs::metadata(filename) {
            Ok(meta) => {
                Some(FileStat {
                    st_mtime: meta.modified().ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                    st_dev: 0,
                    st_ino: 0,
                })
            }
            Err(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// has_valid_path — verify that the containing directory exists
// C: bool has_valid_path(const char *filename)
// ---------------------------------------------------------------------------
pub fn has_valid_path(filename: impl AsRef<Path>) -> bool {
    let path = filename.as_ref();
    let parentdir = usable_parent(path);
    let parentdir_str = parentdir.to_string_lossy();

    // Check if it's the current directory
    if parentdir_str == "." {
        let current = std::env::current_dir();
        let gone = current.is_err();
        if gone {
            statusline(MessageType::Alert, "The working directory has disappeared");
            return false;
        }
    }

    match confined_is_dir(parentdir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            statusline(MessageType::Alert,
                &format!("Directory '{}' does not exist", parentdir_str));
            false
        }
        Err(e) => {
            statusline(MessageType::Alert,
                &format!("Path '{}': {}", parentdir_str, e));
            false
        }
        Ok(is_directory) => {
            if !is_directory {
                statusline(MessageType::Alert,
                    &format!("Path '{}' is not a directory", parentdir_str));
                return false;
            }

            // Opening the parent through the capability already checked
            // traversal permissions without a second ambient resolution.
            if operating_root_is_active() {
                return true;
            }

            // Check for execute access
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                let cpath = match std::ffi::CString::new(parentdir.as_os_str().as_bytes()) {
                    Ok(path) => path,
                    Err(_) => return false,
                };
                let x_ok = unsafe { libc::access(cpath.as_ptr(), libc::X_OK) };
                if x_ok < 0 {
                    statusline(MessageType::Alert,
                        &format!("Path '{}' is not accessible", parentdir_str));
                    return false;
                }
            }

            // Check for write access if locking is enabled
            #[cfg(not(feature = "tiny"))]
            {
                let locking = ISSET!(LOCKING);
                let view_mode = ISSET!(VIEW_MODE);
                if locking && !view_mode {
                    #[cfg(unix)]
                    {
                        use std::os::unix::ffi::OsStrExt;
                        let cpath = match std::ffi::CString::new(parentdir.as_os_str().as_bytes()) {
                            Ok(path) => path,
                            Err(_) => return false,
                        };
                        let w_ok = unsafe { libc::access(cpath.as_ptr(), libc::W_OK) };
                        if w_ok < 0 {
                            statusline(MessageType::Mild,
                                &format!("Directory '{}' is not writable", parentdir_str));
                        }
                    }
                }
            }

            true
        }
    }
}

// ---------------------------------------------------------------------------
// make_new_buffer — add an item to the circular list of openfile structs
// C: void make_new_buffer(void)
// ---------------------------------------------------------------------------
pub fn make_new_buffer() {
    // Allocate before borrowing AppState mutably: LinePtr allocation itself
    // enters the state allocator, and RefCell correctly rejects re-entrancy.
    let filetop = make_new_node(None);
    filetop.borrow_mut().data = String::new();
    filetop.borrow_mut().lineno = 1;

    with_state_mut(|s| {
        let mut newnode = Box::new(OpenFileStruct::default());

        #[cfg(feature = "multibuffer")]
        {
            newnode.seq = s.buffer_seq_counter;
            s.buffer_seq_counter += 1;

            // Link into the circular list: C inserts the new buffer after
            // the current one, so the old current becomes the new one's
            // predecessor (= the back of the ring).
            if let Some(old) = s.openfile.take() {
                s.buffer_ring.push_back(old);

                // More than one buffer: show "Close" in help lines
                if let Some(idx) = s.exitfunc {
                    s.allfuncs[idx].tag = s.close_tag;
                }
                let not_in_help = !s.inhelp;
                if not_in_help || s.more_than_one {
                    s.more_than_one = true;
                }
            }
        }
        #[cfg(not(feature = "multibuffer"))]
        {
            // Without multibuffer there is only ever one buffer.
            s.openfile = None;
        }

        // Initialize fields
        newnode.filename = String::new();

        let filetop_clone = filetop.clone();
        newnode.filetop = Some(filetop);
        newnode.filebot = Some(filetop_clone.clone());
        newnode.current = Some(filetop_clone.clone());
        newnode.current_x = 0;
        newnode.placewewant = 0;
        newnode.brink = 0;
        newnode.cursor_row = 0;
        newnode.edittop = Some(filetop_clone);
        newnode.firstcolumn = 0;
        newnode.totsize = 0;
        newnode.modified = false;

        #[cfg(feature = "wrapping")]
        { newnode.spillage_line = None; }

        #[cfg(not(feature = "tiny"))]
        {
            newnode.mark = None;
            newnode.softmark = false;
            newnode.fmt = FormatType::Unspecified;
            newnode.undotop = None;
            newnode.current_undo = std::ptr::null_mut();
            newnode.last_saved = std::ptr::null_mut();
            newnode.last_action = UndoType::Other;
            newnode.statinfo = None;
            newnode.lock_filename = None;
            newnode.lock_file = None;
        }

        #[cfg(feature = "multibuffer")]
        { newnode.errormessage = None; }

        #[cfg(feature = "color")]
        { newnode.syntax = None; }

        s.openfile = Some(newnode);
    });
}

// ---------------------------------------------------------------------------
// open_buffer — create or populate a buffer from a file
// C: bool open_buffer(const char *filename, bool new_one)
// ---------------------------------------------------------------------------
pub(crate) fn discard_transient_buffer() {
    #[cfg(not(feature = "tiny"))]
    {
        let (lock_filename, lock_path, lock_file) = with_state_mut(|s| match s.openfile.as_mut() {
            Some(buffer) => (
                buffer.lock_filename.take(),
                buffer.lock_path.take(),
                buffer.lock_file.take(),
            ),
            None => (None, None, None),
        });
        if let Some(lock_path) = lock_path {
            delete_lockfile(&lock_path, lock_file.as_ref());
        } else if let Some(lock_filename) = lock_filename {
            delete_lockfile(&lock_filename, lock_file.as_ref());
        }
        drop(lock_file);
    }

    #[cfg(feature = "multibuffer")]
    close_buffer_impl();
    #[cfg(not(feature = "multibuffer"))]
    {
        state_mut().openfile = None;
    }
}

pub fn open_buffer_impl(filename: impl AsRef<Path>, new_one: bool) -> bool {
    let filename = filename.as_ref();
    let filename_given = !filename.as_os_str().is_empty();

    // Display newlines in filenames as ^J
    state_mut().as_an_at = false;

    #[cfg(feature = "operatingdir")]
    {
        let confined = with_state(|s| {
            s.operating_dir.as_deref()
                .map(|_od| outside_of_confinement_path(filename, false))
                .unwrap_or(false)
        });
        if confined {
            let od = state().operating_dir.clone().unwrap_or_default();
            statusline(MessageType::Alert,
                &format!("Can't read file from outside of {}", od));
            return false;
        }
    }

    // The authoritative path; convert to a string only for display.
    let realname = expand_leading_tilde_path(filename);
    let realname_str = printable_path(&realname);

    // Don't try to open directories, character files, or block files.
    if filename_given {
        if let Ok(info) = path_info(&realname) {
            if info.is_dir {
                statusline(MessageType::Alert, &format!("\"{}\" is a directory", realname_str));
                return false;
            }
            // Check for block/char device and FIFO (requires libc)
            if info.is_special {
                statusline(MessageType::Alert, &format!("\"{}\" is a device file", realname_str));
                return false;
            }
            #[cfg(feature = "tiny")]
            if info.is_fifo {
                statusline(MessageType::Alert, &format!("\"{}\" is a FIFO", realname_str));
                return false;
            }
            #[cfg(all(not(feature = "tiny"), unix))]
            {
                let euid = unsafe { libc::geteuid() };
                if new_one && (info.mode & 0o222) == 0 && euid == ROOT_UID {
                    statusline(MessageType::Alert,
                        &format!("{} is meant to be read-only", realname_str));
                }
            }
        }
    }

    if new_one {
        make_new_buffer();

        if has_valid_path(&realname) {
            #[cfg(not(feature = "tiny"))]
            {
                let do_locking = ISSET!(LOCKING);
                let view_mode = ISSET!(VIEW_MODE);
                if do_locking && !view_mode && filename_given {
                    match do_lockfile(&realname, true) {
                        Err(()) => {
                            // SKIPTHISFILE
                            #[cfg(feature = "multibuffer")]
                            close_buffer_impl();
                            return false;
                        }
                        Ok(lock) => {
                            with_state_mut(|s| {
                                if let Some(ref mut of) = s.openfile {
                                    if let Some((lock_path, lock_file)) = lock {
                                        of.lock_filename = Some(printable_path(&lock_path));
                                        of.lock_path = Some(lock_path);
                                        of.lock_file = Some(lock_file);
                                    }
                                }
                            });
                        }
                    }
                }
            }
        }
    }

    // If we have a filename and are not in NOREAD mode, open the file.
    let noread = ISSET!(NOREAD_MODE);
    let mut descriptor: i32 = 0;
    let mut file_handle: Option<File> = None;

    if filename_given && !noread {
        descriptor = open_file_impl(&realname, new_one, &mut file_handle);
    }

    // If successfully opened an existing file, read it in.
    if descriptor > 0 {
        if let Some(f) = file_handle {
            install_handler_for_Ctrl_C();
            let read_succeeded = read_file_impl(f, true, &realname, !new_one);
            restore_handler_for_Ctrl_C();
            if !read_succeeded {
                if new_one {
                    discard_transient_buffer();
                }
                return false;
            }

            #[cfg(not(feature = "tiny"))]
            {
                let has_stat = with_state(|s| {
                    s.openfile.as_ref().map(|of| of.statinfo.is_some()).unwrap_or(false)
                });
                if !has_stat {
                    let st = stat_with_alloc(&realname);
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.statinfo = st;
                        }
                    });
                }
            }
        }
    } else if descriptor < 0 {
        if new_one {
            discard_transient_buffer();
        }
        return false;
    }

    // For a new buffer, store filename and put cursor at start of buffer.
    if descriptor >= 0 && new_one {
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                set_openfile_filename(of, realname.clone());
                let filetop = of.filetop.clone();
                of.current = filetop;
                of.current_x = 0;
                of.placewewant = 0;
            }
        });
    }

    #[cfg(feature = "color")]
    if new_one {
        find_and_prime_applicable_syntax();
    }

    true
}

// ---------------------------------------------------------------------------
// set_modified — mark the current buffer as modified
// C: void set_modified(void)
// ---------------------------------------------------------------------------
pub fn set_modified() {
    let already = with_state(|s| s.openfile.as_ref().map(|of| of.modified).unwrap_or(false));
    if already {
        return;
    }
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.modified = true;
        }
    });
    titlebar(None);

    #[cfg(not(feature = "tiny"))]
    {
        let (mut lock_file, lock_path, buf_path) = with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                let lock_path = of.lock_path.clone()
                    .or_else(|| of.lock_filename.as_deref().map(PathBuf::from));
                let buf_path = match openfile_filename_path(of) {
                    Some(path) => path.to_path_buf(),
                    None => PathBuf::from(&of.filename),
                };
                (of.lock_file.take(), lock_path, buf_path)
            } else {
                (None, None, PathBuf::new())
            }
        });
        let keep_lock = match (lock_file.as_mut(), lock_path.as_deref()) {
            (Some(file), Some(path)) => {
                write_lockfile(file, path, &buf_path, true)
            }
            (None, None) => true,
            _ => false,
        };
        if !keep_lock {
            lock_file = None;
        }
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                if keep_lock && of.lock_file.is_none() {
                    of.lock_file = lock_file;
                } else if !keep_lock {
                    of.lock_filename = None;
                    of.lock_path = None;
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// prepare_for_display — update title bar and multiline cache
// C: void prepare_for_display(void)
// ---------------------------------------------------------------------------
pub fn prepare_for_display() {
    let inhelp = state().inhelp;
    if !inhelp {
        titlebar(None);
    }

    #[cfg(feature = "color")]
    {
        let needs_precalc = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.filetop.as_ref())
                .map(|ft| ft.borrow().multidata.is_empty())
                .unwrap_or(false)
        });
        if needs_precalc {
            precalc_multicolorinfo();
        }
        state_mut().have_palette = false;
    }
    state_mut().refresh_needed = true;
}

// ---------------------------------------------------------------------------
// MULTIBUFFER functions
// ---------------------------------------------------------------------------

/* C: void mention_name_and_linecount(void)
 * Show name of current buffer and its number of lines on the status bar. */
#[cfg(feature = "multibuffer")]
pub fn mention_name_and_linecount() {
    let (count, filename) = with_state(|s| {
        if let Some(ref of) = s.openfile {
            let bot_lineno = of.filebot.as_ref().map(|b| b.borrow().lineno).unwrap_or(0);
            let bot_empty = of.filebot.as_ref()
                .map(|b| b.borrow().data.is_empty())
                .unwrap_or(true);
            let count = bot_lineno - (if bot_empty { 1 } else { 0 });
            (count as usize, of.filename.clone())
        } else {
            (0, String::new())
        }
    });

    #[cfg(not(feature = "tiny"))]
    {
        let minibar = ISSET!(MINIBAR);
        let zero = ISSET!(ZERO);
        if minibar {
            state_mut().report_size = true;
            return;
        }
        if zero {
            return;
        }
        let fmt = with_state(|s| {
            s.openfile.as_ref().map(|of| of.fmt).unwrap_or(FormatType::Unspecified)
        });
        if fmt == FormatType::DosFile {
            let name = if filename.is_empty() { "New Buffer" } else { &filename };
            let msg = if count == 1 {
                format!("{} -- {} line ({})", name, count, "DOS")
            } else {
                format!("{} -- {} lines ({})", name, count, "DOS")
            };
            statusline(MessageType::Hush, &msg);
            return;
        }
    }

    let name = if filename.is_empty() { "New Buffer" } else { &filename };
    let msg = if count == 1 {
        format!("{} -- {} line", name, count)
    } else {
        format!("{} -- {} lines", name, count)
    };
    statusline(MessageType::Hush, &msg);
}

/* C: void redecorate_after_switch(void)
 * Update title bar and such after switching to another buffer. */
#[cfg(feature = "multibuffer")]
pub fn redecorate_after_switch() {
    // If only one file buffer is open, there is nothing to update.
    if state().buffer_ring.is_empty() {
        statusline(MessageType::Ahem, "No more open file buffers");
        return;
    }

    // While in a different buffer, the width of the screen may have changed,
    // so make sure that the starting column for the first row is fitting.
    #[cfg(not(feature = "tiny"))]
    ensure_firstcolumn_is_aligned();

    prepare_for_display();

    with_state_mut(|s| {
        s.currmenu = MMOST;
        s.shift_held = true;
    });

    let (has_error, error_msg) = with_state(|s| {
        if let Some(ref of) = s.openfile {
            (of.errormessage.is_some(), of.errormessage.clone())
        } else {
            (false, None)
        }
    });

    if has_error {
        if let Some(msg) = error_msg {
            statusline(MessageType::Alert, &msg);
        }
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.errormessage = None;
            }
        });
    } else {
        mention_name_and_linecount();
    }
}

/* C: void switch_to_prev_buffer(void)
 * Switch to the previous entry in the circular list of buffers. */
#[cfg(feature = "multibuffer")]
pub fn switch_to_prev_buffer() {
    with_state_mut(|s| {
        if let Some(prev) = s.buffer_ring.pop_back() {
            if let Some(cur) = s.openfile.replace(prev) {
                s.buffer_ring.push_front(cur);
            }
        }
    });
    redecorate_after_switch();
}

/* C: void switch_to_next_buffer(void)
 * Switch to the next entry in the circular list of buffers. */
#[cfg(feature = "multibuffer")]
pub fn switch_to_next_buffer() {
    with_state_mut(|s| {
        if let Some(next) = s.buffer_ring.pop_front() {
            if let Some(cur) = s.openfile.replace(next) {
                s.buffer_ring.push_back(cur);
            }
        }
    });
    redecorate_after_switch();
}

/// Rotate the buffer ring (without redecorating) until the buffer with the
/// given filename is current.  Returns false (with the original buffer
/// restored as current) when no open buffer has that name.
/// C equivalent: the openfile->next walk in do_linter.
#[cfg(feature = "multibuffer")]
pub fn rotate_to_buffer_named(name: &str) -> bool {
    let total = with_state(|s| s.buffer_ring.len() + usize::from(s.openfile.is_some()));
    for _ in 0..total {
        let matches = with_state(|s| {
            s.openfile.as_ref().map(|f| f.filename == name).unwrap_or(false)
        });
        if matches {
            return true;
        }
        with_state_mut(|s| {
            if let Some(next) = s.buffer_ring.pop_front() {
                if let Some(cur) = s.openfile.replace(next) {
                    s.buffer_ring.push_back(cur);
                }
            }
        });
    }
    false
}

/* C: void close_buffer(void)
 * Remove the current buffer from the circular list of buffers;
 * the preceding buffer becomes the current one (like C's
 * `openfile = orphan->prev`), or None when it was the last. */
#[cfg(feature = "multibuffer")]
pub fn close_buffer_impl() {
    with_state_mut(|s| {
        // Dropping the Box frees the lines, undo stack, and metadata.
        let _orphan = s.openfile.take();
        s.openfile = s.buffer_ring.pop_back();

        // When just one buffer remains open, show "Exit" in the help lines.
        if s.buffer_ring.is_empty() {
            if let Some(idx) = s.exitfunc {
                s.allfuncs[idx].tag = s.exit_tag;
            }
        }
    });
}

// ---------------------------------------------------------------------------
// encode_data — encode NUL bytes in a buffer as LF bytes
// C: char *encode_data(char *text, size_t length)
// ---------------------------------------------------------------------------
pub fn encode_data(buf: &[u8]) -> (String, bool) {
    // Fast path: the overwhelmingly common case is a line with no NUL bytes, so
    // skip the recode allocation entirely and validate the slice in place.
    if !buf.contains(&0) {
        return match std::str::from_utf8(buf) {
            Ok(s) => (s.to_owned(), false),
            Err(_) => (String::from_utf8_lossy(buf).into_owned(), true),
        };
    }
    // NUL present: replace NUL bytes with LF (0x0A) as in C's recode_NUL_to_LF,
    // then decode (lossily, matching the previous behaviour exactly).
    let recoded: Vec<u8> = buf.iter().map(|&b| if b == 0 { b'\n' } else { b }).collect();
    match String::from_utf8(recoded) {
        Ok(s) => (s, false),
        Err(e) => {
            let bytes = e.into_bytes();
            (String::from_utf8_lossy(&bytes).into_owned(), true)
        }
    }
}

fn read_until_cancelled<R: Read>(
    reader: &mut R,
    cancelled: &std::sync::atomic::AtomicBool,
) -> io::Result<(Vec<u8>, bool)> {
    let mut content = Vec::new();
    let mut chunk = [0u8; 16 * 1024];

    loop {
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok((content, true));
        }

        match reader.read(&mut chunk) {
            Ok(0) => return Ok((content, false)),
            Ok(amount) => content.extend_from_slice(&chunk[..amount]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                    return Ok((content, true));
                }
                // EINTR without a cancellation request is transient.
            }
            Err(error) => return Err(error),
        }
    }
}

// ---------------------------------------------------------------------------
// read_file_impl — read an open file into the current buffer
// C: void read_file(FILE *f, int fd, const char *filename, bool undoable)
// ---------------------------------------------------------------------------
pub fn read_file_impl<R: Read>(
    mut f: R,
    had_real_fd: bool,
    filename: impl AsRef<Path>,
    undoable: bool,
) -> bool {
    let filename = filename.as_ref();
    let was_lineno = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.current.as_ref())
            .map(|c| c.borrow().lineno)
            .unwrap_or(1)
    });

    let was_leftedge: usize;
    #[cfg(not(feature = "tiny"))]
    {
        was_leftedge = if ISSET!(SOFTWRAP) {
            let cur = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.current.clone())
            });
            leftedge_for(xplustabs(), cur.as_ref())
        } else {
            0
        };
    }
    #[cfg(feature = "tiny")]
    { was_leftedge = 0; }

    let topline = make_new_node(None);
    topline.borrow_mut().lineno = 1;
    let mut bottomline = topline.clone();
    let mut num_lines: usize = 0;

    let mut error_occurred = false;
    let mut error_msg = String::new();

    #[cfg(not(feature = "tiny"))]
    block_sigwinch(true);

    crate::nano::CONTROL_C_WAS_PRESSED.store(
        false,
        std::sync::atomic::Ordering::SeqCst,
    );

    // Read in bounded chunks so a SIGINT interruption is observed between
    // reads.  The signal handler publishes only to the atomic flag.
    let (content, interrupted) = match read_until_cancelled(
        &mut f,
        &crate::nano::CONTROL_C_WAS_PRESSED,
    ) {
        Ok(result) => result,
        Err(error) => {
            error_occurred = true;
            error_msg = error.to_string();
            (Vec::new(), false)
        }
    };
    let mut had_invalid_utf8 = false;

    #[cfg(not(feature = "tiny"))]
    block_sigwinch(false);

    #[cfg(not(feature = "tiny"))]
    {
        if isendwin() {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                reconnect_and_store_state();
            }
            terminal_init();
            doupdate();
        }
    }

    // Keep the legacy field synchronized for callers that inspect it, but never
    // mutate AppState from the asynchronous handler itself.
    state_mut().control_C_was_pressed = interrupted;
    if error_occurred {
        statusline(MessageType::Alert, &error_msg);
        return false;
    }
    if interrupted {
        statusline(MessageType::Alert, "Interrupted");
        // Do not ingraft a partially read file/FIFO.  The caller's pre-existing
        // buffer remains the only observable state.
        return false;
    }

    // Check writability
    let writable = if had_real_fd && !undoable && !ISSET!(VIEW_MODE) {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let cname = std::ffi::CString::new(filename.as_os_str().as_bytes())
                .unwrap_or_default();
            let access_ret = unsafe { libc::access(cname.as_ptr(), libc::W_OK) };
            access_ret == 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    } else {
        true
    };

    // Parse the content into lines. Walk newline-to-newline over slices of the
    // already-read buffer instead of copying byte-by-byte into a scratch Vec; the
    // newline search auto-vectorizes and each line is sliced, not re-copied.
    #[cfg(not(feature = "tiny"))]
    let mut format = FormatType::NixFile;

    // Pre-size the line arena from the file length so the slab Vec doesn't have to
    // grow-and-memcpy repeatedly while reading (avg line ~48 bytes incl. newline).
    state_mut().lines.reserve(content.len() / 48 + 8);

    let mut start = 0usize;
    while start < content.len() {
        let nl = match content[start..].iter().position(|&b| b == b'\n') {
            Some(rel) => start + rel,
            None => break, // no further newline; trailing bytes handled below
        };
        let mut line: &[u8] = &content[start..nl];
        #[cfg(not(feature = "tiny"))]
        {
            // Strip a CR immediately before the LF (DOS line ending).
            if line.last() == Some(&b'\r') && !ISSET!(NO_CONVERT) {
                if num_lines == 0 {
                    format = FormatType::DosFile;
                }
                line = &line[..line.len() - 1];
            }
        }
        let (data, invalid_utf8) = encode_data(line);
        had_invalid_utf8 |= invalid_utf8;
        bottomline.borrow_mut().data = data;

        // Make a new node for the next line.
        let newline = make_new_node(Some(bottomline.clone()));
        newline.borrow_mut().lineno = (num_lines + 2) as isize;
        bottomline.borrow_mut().next = Some(newline.clone());
        bottomline = newline;
        num_lines += 1;
        start = nl + 1;
    }

    // Handle the final segment after the last newline (may be empty when the file
    // ends in '\n').
    let tail: &[u8] = &content[start..];
    if tail.is_empty() {
        bottomline.borrow_mut().data = String::new();
    } else {
        let (data, invalid_utf8) = encode_data(tail);
        had_invalid_utf8 |= invalid_utf8;
        bottomline.borrow_mut().data = data;
        num_lines += 1;
    }

    // Capture the undo origin only after the complete input is available; a
    // failed or cancelled read must not leave a phantom undo entry.
    #[cfg(not(feature = "tiny"))]
    if undoable {
        add_undo(UndoType::Insert, None);
    }

    // Insert the read buffer into the current buffer
    ingraft_buffer(topline);

    // Set the desired x position at the end of what was inserted
    let xpt = xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = xpt;
            of.had_invalid_utf8 |= had_invalid_utf8;
        }
    });

    if !writable {
        statusline(MessageType::Alert,
            &format!("File '{}' is unwritable", printable_path(filename)));
    } else {
        let zero = ISSET!(ZERO);
        let minibar = ISSET!(MINIBAR);
        let we_are_running = state().we_are_running;
        if (zero || minibar) && !(we_are_running && undoable) {
            // No blurb for new buffers with --zero or --mini
        } else {
            #[cfg(not(feature = "tiny"))]
            if format == FormatType::DosFile {
                let msg = if num_lines == 1 {
                    format!("Read {} line (converted from DOS format)", num_lines)
                } else {
                    format!("Read {} lines (converted from DOS format)", num_lines)
                };
                statusline(MessageType::Remark, &msg);
            } else {
                let msg = if num_lines == 1 {
                    format!("Read {} line", num_lines)
                } else {
                    format!("Read {} lines", num_lines)
                };
                statusline(MessageType::Remark, &msg);
            }
            #[cfg(feature = "tiny")]
            {
                let msg = if num_lines == 1 {
                    format!("Read {} line", num_lines)
                } else {
                    format!("Read {} lines", num_lines)
                };
                statusline(MessageType::Remark, &msg);
            }
        }
    }

    state_mut().report_size = true;
    if had_invalid_utf8 {
        statusline(
            MessageType::Alert,
            "File contains invalid UTF-8; normal save is disabled",
        );
    }

    // If we inserted less than a screenful, don't center the cursor.
    if undoable && less_than_a_screenful(was_lineno, was_leftedge) {
        state_mut().focusing = false;
        #[cfg(feature = "color")]
        with_state_mut(|s| s.perturbed = true);
    } else if undoable {
        #[cfg(feature = "color")]
        with_state_mut(|s| s.recook = true);
    }

    #[cfg(not(feature = "tiny"))]
    {
        if undoable {
            update_undo(UndoType::Insert);
        }
        let make_it_unix = ISSET!(MAKE_IT_UNIX);
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                if make_it_unix {
                    of.fmt = FormatType::NixFile;
                } else if of.fmt == FormatType::Unspecified {
                    of.fmt = format;
                }
            }
        });
    }
    true
}

// ---------------------------------------------------------------------------
// open_file_impl — open the file with the given name
// C: int open_file(const char *filename, bool new_one, FILE **f)
// Returns 0=new file, -1=failure, or the file descriptor on success.
// ---------------------------------------------------------------------------
pub fn open_file_impl(filename: &Path, new_one: bool, out_file: &mut Option<File>) -> i32 {
    let full_filename = get_full_path_buf(filename);
    let resolved: PathBuf = full_filename.unwrap_or_else(|| filename.to_path_buf());

    // Check if it's a FIFO
    #[cfg(not(feature = "tiny"))]
    {
        if let Ok(info) = path_info(&resolved) {
            if info.is_fifo {
                statusbar("Reading from FIFO...");
            }
        }
        block_sigwinch(true);
        install_handler_for_Ctrl_C();
    }

    // Open the file
    // This is the authority-bearing read.  When operating-directory mode is
    // active it resolves and opens beneath the retained root in one operation;
    // the earlier display/metadata checks are never trusted for confinement.
    let open_result = open_path(&resolved);

    #[cfg(not(feature = "tiny"))]
    {
        restore_handler_for_Ctrl_C();
        block_sigwinch(false);
    }

    match open_result {
        Err(e) => {
            let err_kind = e.kind();
            if err_kind == io::ErrorKind::NotFound && new_one {
                statusline(MessageType::Remark, "New File");
                0
            } else if err_kind == io::ErrorKind::NotFound {
                statusline(MessageType::Alert,
                    &format!("File \"{}\" not found", printable_path(filename)));
                -1
            } else if err_kind == io::ErrorKind::Interrupted {
                statusline(MessageType::Alert, "Interrupted");
                -1
            } else {
                statusline(MessageType::Alert,
                    &format!("Error reading {}: {}", printable_path(filename), e));
                -1
            }
        }
        Ok(f) => {
            // Get the file descriptor
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd;
                let _fd = f.as_raw_fd();
            }

            let zero = ISSET!(ZERO);
            let we_are_running = state().we_are_running;
            if !zero || we_are_running {
                statusbar("Reading...");
            }

            // Store file handle; we use fd 1 to signal success (actual fd varies)
            *out_file = Some(f);
            1  // signal success; caller uses the file handle
        }
    }
}

// ---------------------------------------------------------------------------
// get_next_filename — return the first available extension of a filename
// C: char *get_next_filename(const char *name, const char *suffix)
// ---------------------------------------------------------------------------
pub fn get_next_filename(name: &str, suffix: &str) -> String {
    let base = format!("{}{}", name, suffix);

    match path_exists_nofollow(Path::new(&base)) {
        Ok(false) => return base,
        Ok(true) => {}
        Err(_) => return String::new(),
    }

    for i in 1u64..100_000 {
        let candidate = format!("{}.{}", base, i);
        match path_exists_nofollow(Path::new(&candidate)) {
            Ok(false) => return candidate,
            Ok(true) => {}
            Err(_) => return String::new(),
        }
    }

    // No possible save file
    String::new()
}

// ---------------------------------------------------------------------------
// Process / fork / pipe support (!NANO_TINY)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "tiny"))]
use std::sync::atomic::{AtomicI32, Ordering};

#[cfg(not(feature = "tiny"))]
static PID_OF_COMMAND: AtomicI32 = AtomicI32::new(-1);
#[cfg(not(feature = "tiny"))]
static PID_OF_SENDER: AtomicI32 = AtomicI32::new(-1);
#[cfg(not(feature = "tiny"))]
static SHOULD_PIPE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(not(feature = "tiny"), unix))]
struct CommandSession {
    old_sigint: libc::sigaction,
    old_termios: Option<libc::termios>,
    terminal_was_left: bool,
}

#[cfg(all(not(feature = "tiny"), unix))]
impl CommandSession {
    fn start(leave_terminal: bool) -> io::Result<Self> {
        let mut old_sigint: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &mut old_sigint) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        let old_termios = if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut termios) } == 0 {
            Some(termios)
        } else {
            None
        };

        if leave_terminal {
            crate::nano::restore_terminal();
        }

        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = cancel_command_trampoline as *const () as libc::sighandler_t;
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        action.sa_flags = 0;
        if unsafe { libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) } != 0 {
            if leave_terminal {
                terminal_init();
            }
            return Err(io::Error::last_os_error());
        }

        enable_kb_interrupt();
        Ok(Self {
            old_sigint,
            old_termios,
            terminal_was_left: leave_terminal,
        })
    }

    fn resume_editor_terminal(&mut self) {
        if self.terminal_was_left {
            terminal_init();
            state_mut().refresh_needed = true;
            self.terminal_was_left = false;
        }
    }
}

#[cfg(all(not(feature = "tiny"), unix))]
impl Drop for CommandSession {
    fn drop(&mut self) {
        PID_OF_COMMAND.store(-1, Ordering::SeqCst);
        PID_OF_SENDER.store(-1, Ordering::SeqCst);
        SHOULD_PIPE.store(false, Ordering::SeqCst);

        unsafe {
            libc::sigaction(libc::SIGINT, &self.old_sigint, std::ptr::null_mut());
        }
        if self.terminal_was_left {
            terminal_init();
            state_mut().refresh_needed = true;
        }
        if let Some(settings) = self.old_termios.as_ref() {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, settings);
            }
        }
    }
}

/* C: void cancel_the_command(int signal)
 * Send an unconditional kill signal to the running external command. */
#[cfg(not(feature = "tiny"))]
pub fn cancel_the_command(_signal: i32) {
    #[cfg(unix)]
    {
        let pid_cmd = PID_OF_COMMAND.load(Ordering::SeqCst);
        let pid_snd = PID_OF_SENDER.load(Ordering::SeqCst);
        let piping = SHOULD_PIPE.load(Ordering::SeqCst);

        if pid_cmd > 0 {
            unsafe {
                // The command is its own process group, so descendants do not
                // survive a cancelled shell.  Fall back to the leader in case
                // setpgid lost a short spawn race.
                if libc::kill(-pid_cmd, libc::SIGKILL) != 0 {
                    libc::kill(pid_cmd, libc::SIGKILL);
                }
            }
        }
        if piping && pid_snd > 0 {
            unsafe { libc::kill(pid_snd, libc::SIGKILL); }
        }
    }
    #[cfg(not(unix))]
    {
        // No-op on non-Unix platforms
    }
}

#[cfg(all(not(feature = "tiny"), unix))]
extern "C" fn cancel_command_trampoline(signal: libc::c_int) {
    cancel_the_command(signal);
}

#[cfg(all(not(feature = "tiny"), unix))]
fn wait_for_command(pid: libc::pid_t) -> io::Result<i32> {
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return Ok(status);
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
}

/* C: void send_data(const linestruct *line, int fd)
 * Send the text that starts at the given line to file descriptor fd. */
#[cfg(not(feature = "tiny"))]
pub fn send_data(line: Option<LinePtr>, fd: i32) {
    #[cfg(unix)]
    {
        use std::os::unix::io::FromRawFd;
        let mut tube = unsafe { File::from_raw_fd(fd) };

        let mut current = line;
        while let Some(node) = current {
            let (data, has_next) = {
                let b = node.borrow();
                (b.data.clone(), b.next.is_some())
            };

            // Don't write a final empty line
            let next_node = node.borrow().next.clone();
            if next_node.is_none() && data.is_empty() {
                break;
            }

            // Recode LF as NUL before writing
            let recoded: Vec<u8> = data.bytes().map(|b| if b == b'\n' { 0 } else { b }).collect();
            if tube.write_all(&recoded).is_err() {
                std::process::exit(5);
            }

            if has_next {
                if tube.write_all(b"\n").is_err() {
                    std::process::exit(6);
                }
            }

            current = node.borrow().next.clone();
        }
        // Don't close here — the fd will be closed by the OS when the process exits
        std::mem::forget(tube);
    }
    #[cfg(not(unix))]
    {
        // No-op on non-Unix platforms
    }
}

#[cfg(not(feature = "tiny"))]
fn append_pipe_text(output: &mut Vec<u8>, text: &str) {
    output.extend(text.bytes().map(|byte| if byte == b'\n' { 0 } else { byte }));
}

#[cfg(not(feature = "tiny"))]
fn command_input_snapshot() -> Vec<u8> {
    let marked = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|buffer| buffer.mark.as_ref())
            .is_some()
    });
    let mut output = Vec::new();

    if marked {
        let mut top = None;
        let mut top_x = 0;
        let mut bottom = None;
        let mut bottom_x = 0;
        get_region(&mut top, &mut top_x, &mut bottom, &mut bottom_x);
        let (top, bottom) = match (top, bottom) {
            (Some(top), Some(bottom)) => (top, bottom),
            _ => return output,
        };

        let mut line = Some(top.clone());
        while let Some(node) = line {
            let is_top = node == top;
            let is_bottom = node == bottom;
            let (data, next) = {
                let borrowed = node.borrow();
                (borrowed.data.clone(), borrowed.next.clone())
            };
            let start = if is_top { top_x } else { 0 };
            let end = if is_bottom { bottom_x } else { data.len() };
            if let Some(segment) = data.get(start..end) {
                append_pipe_text(&mut output, segment);
            }
            if is_bottom {
                break;
            }
            output.push(b'\n');
            line = next;
        }
        return output;
    }

    let mut line = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|buffer| buffer.filetop.clone())
    });
    while let Some(node) = line {
        let (data, next) = {
            let borrowed = node.borrow();
            (borrowed.data.clone(), borrowed.next.clone())
        };
        if next.is_none() && data.is_empty() {
            break;
        }
        append_pipe_text(&mut output, &data);
        if next.is_some() {
            output.push(b'\n');
        }
        line = next;
    }
    output
}

#[cfg(all(not(feature = "tiny"), unix))]
fn send_snapshot(data: &[u8], fd: i32) -> ! {
    use std::os::unix::io::FromRawFd;

    let mut pipe = unsafe { File::from_raw_fd(fd) };
    let status = if pipe.write_all(data).is_ok() { 0 } else { 5 };
    drop(pipe);
    unsafe { libc::_exit(status) }
}

/* C: void execute_command(const char *command)
 * Execute the given command in a shell. */
#[cfg(not(feature = "tiny"))]
pub fn execute_command(command: &str) {
    #[cfg(unix)]
    {
        
        use std::os::unix::io::{AsRawFd, FromRawFd};

        let should_pipe = command.starts_with('|');
        let capture_output = !(should_pipe && command.len() > 1 && command.chars().nth(1) == Some('|'));
        let filter_mode = should_pipe && capture_output;

        SHOULD_PIPE.store(should_pipe, Ordering::SeqCst);

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

        // The actual command string passed to the shell
        let cmd_str = if should_pipe {
            if capture_output { &command[1..] } else { &command[2..] }
        } else {
            command
        };
        let input_was_marked = should_pipe && with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|buffer| buffer.mark.as_ref())
                .is_some()
        });
        let command_input = if should_pipe {
            Some(command_input_snapshot())
        } else {
            None
        };

        // A filter writes stdout and stderr to separate secure staging files.
        // The document is replaced only after both child processes succeed.
        let mut filter_stdout = if filter_mode {
            match tempfile::NamedTempFile::new() {
                Ok(file) => Some(file),
                Err(error) => {
                    statusline(MessageType::Alert,
                        &format!("Could not create filter output: {}", error));
                    return;
                }
            }
        } else {
            None
        };
        let mut filter_stderr = if filter_mode {
            match tempfile::NamedTempFile::new() {
                Ok(file) => Some(file),
                Err(error) => {
                    statusline(MessageType::Alert,
                        &format!("Could not create filter diagnostics: {}", error));
                    return;
                }
            }
        } else {
            None
        };

        // Non-filter commands stream captured output directly into the editor.
        let (from_read_fd, from_write_fd) = if filter_mode {
            (-1, -1)
        } else {
            let mut fds = [0i32; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                statusline(MessageType::Alert,
                    &format!("Could not create pipe: {}", io::Error::last_os_error()));
                return;
            }
            (fds[0], fds[1])
        };

        // Create to_fd pipe (input to command) if piping
        let (to_read_fd, to_write_fd) = if should_pipe {
            let mut fds = [0i32; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                statusline(MessageType::Alert,
                    &format!("Could not create pipe: {}", io::Error::last_os_error()));
                if !filter_mode {
                    unsafe { libc::close(from_read_fd); libc::close(from_write_fd); }
                }
                return;
            }
            (fds[0], fds[1])
        } else {
            (-1, -1)
        };

        statusbar("Executing...");
        let mut session = match CommandSession::start(!capture_output) {
            Ok(session) => session,
            Err(error) => {
                statusline(MessageType::Alert,
                    &format!("Could not prepare command session: {}", error));
                unsafe {
                    if !filter_mode {
                        libc::close(from_read_fd);
                        libc::close(from_write_fd);
                    }
                    if should_pipe {
                        libc::close(to_read_fd);
                        libc::close(to_write_fd);
                    }
                }
                return;
            }
        };

        // Fork the child process
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // Child process
            unsafe {
                libc::setpgid(0, 0);
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                if filter_mode {
                    libc::dup2(
                        filter_stdout.as_ref().unwrap().as_file().as_raw_fd(),
                        libc::STDOUT_FILENO,
                    );
                    libc::dup2(
                        filter_stderr.as_ref().unwrap().as_file().as_raw_fd(),
                        libc::STDERR_FILENO,
                    );
                } else {
                    libc::close(from_read_fd);
                    if capture_output {
                        libc::dup2(from_write_fd, libc::STDOUT_FILENO);
                    }
                    libc::dup2(from_write_fd, libc::STDERR_FILENO);
                    libc::close(from_write_fd);
                }

                if should_pipe {
                    libc::dup2(to_read_fd, libc::STDIN_FILENO);
                    libc::close(to_read_fd);
                    libc::close(to_write_fd);
                }

                let shell_cstr = std::ffi::CString::new(shell.as_str()).unwrap();
                let shell_tail = std::ffi::CString::new(
                    crate::utils::tail(&shell)).unwrap();
                let minus_c = std::ffi::CString::new("-c").unwrap();
                let cmd_cstr = std::ffi::CString::new(cmd_str).unwrap();

                libc::execl(
                    shell_cstr.as_ptr(),
                    shell_tail.as_ptr(),
                    minus_c.as_ptr(),
                    cmd_cstr.as_ptr(),
                    std::ptr::null::<libc::c_char>(),
                );
                libc::_exit(6);
            }
        }

        // Parent
        if !filter_mode {
            unsafe { libc::close(from_write_fd); }
        }

        if pid < 0 {
            statusline(MessageType::Alert,
                &format!("Could not fork: {}", io::Error::last_os_error()));
            if !filter_mode {
                unsafe { libc::close(from_read_fd); }
            }
            if should_pipe {
                unsafe { libc::close(to_read_fd); libc::close(to_write_fd); }
            }
            return;
        }

        PID_OF_COMMAND.store(pid, Ordering::SeqCst);
        unsafe {
            // Also set the group from the parent to close the child-side race.
            libc::setpgid(pid, pid);
        }

        // If the command starts with "|", pipe buffer or region to the command.
        let pid_sender;
        if should_pipe {
            pid_sender = unsafe { libc::fork() };
            if pid_sender == 0 {
                // Child sender process
                unsafe {
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                    libc::close(to_read_fd);
                }
                send_snapshot(command_input.as_deref().unwrap_or_default(), to_write_fd);
            }

            if pid_sender < 0 {
                statusline(MessageType::Alert,
                    &format!("Could not fork: {}", io::Error::last_os_error()));
            }

            if pid_sender > 0 {
                PID_OF_SENDER.store(pid_sender, Ordering::SeqCst);
            }
            unsafe { libc::close(to_read_fd); libc::close(to_write_fd); }
        } else {
            pid_sender = -1;
        }

        // Ordinary execute commands insert their output.  Filters keep stdout
        // staged until the child and input-sender have both succeeded.
        let output_was_ingrafted = if !filter_mode {
            let stream = unsafe { File::from_raw_fd(from_read_fd) };
            read_file_impl(stream, false, "pipe", true)
        } else {
            false
        };

        // Wait for processes
        let (command_status, command_waited) = match wait_for_command(pid) {
            Ok(status) => (status, true),
            Err(error) => {
                statusline(MessageType::Alert, &format!("Could not wait for command: {}", error));
                (0, false)
            }
        };

        let mut sender_status: i32 = 0;
        let mut sender_waited = !should_pipe;
        if should_pipe && pid_sender > 0 {
            match wait_for_command(pid_sender) {
                Ok(status) => {
                    sender_status = status;
                    sender_waited = true;
                }
                Err(error) => statusline(
                    MessageType::Alert,
                    &format!("Could not wait for pipe sender: {}", error),
                ),
            }
        }

        session.resume_editor_terminal();
        drop(session);

        // Check exit status
        let cmd_ok = command_waited
            && libc::WIFEXITED(command_status)
            && libc::WEXITSTATUS(command_status) == 0;
        let cmd_signaled = libc::WIFSIGNALED(command_status);
        let sender_ok = !should_pipe
            || (pid_sender > 0
                && sender_waited
                && libc::WIFEXITED(sender_status)
                && libc::WEXITSTATUS(sender_status) == 0);

        if !cmd_ok {
            if cmd_signaled {
                statusline(MessageType::Alert, "Cancelled");
            } else {
                let err_detail = if filter_mode {
                    let mut bytes = Vec::new();
                    if let Some(diagnostics) = filter_stderr.as_mut() {
                        let _ = diagnostics.as_file_mut().seek(SeekFrom::Start(0));
                        let _ = diagnostics.as_file_mut().take(16 * 1024).read_to_end(&mut bytes);
                    }
                    let text = String::from_utf8_lossy(&bytes);
                    text.lines().next().unwrap_or("---").to_string()
                } else {
                    // Try to extract an error message from the inserted output.
                    with_state(|s| {
                        s.openfile.as_ref()
                            .and_then(|of| of.current.as_ref())
                            .and_then(|c| {
                                let b = c.borrow();
                                b.prev.as_ref()
                                    .and_then(|pw| pw.upgrade())
                                    .map(|prev| {
                                        let pb = prev.borrow();
                                        if let Some(pos) = pb.data.find(": ") {
                                            pb.data[pos + 2..].to_string()
                                        } else {
                                            "---".to_string()
                                        }
                                    })
                            })
                            .unwrap_or_else(|| "---".to_string())
                    })
                };
                statusline(MessageType::Alert, &format!("Error: {}", err_detail));
            }
        } else if !sender_ok {
            statusline(MessageType::Alert, "Piping failed");
        } else if filter_mode {
            let output = filter_stdout.as_mut().expect("filter stdout staging");
            let staged_ok = output
                .as_file_mut()
                .flush()
                .and_then(|_| output.as_file().sync_all());
            if let Err(error) = staged_ok {
                statusline(MessageType::Alert, &format!("Could not sync filter output: {}", error));
            } else {
                let output_path = output.path().to_string_lossy().into_owned();
                let action = if input_was_marked {
                    UndoType::Cut
                } else {
                    UndoType::CutToEof
                };
                if crate::text::replace_buffer(&output_path, action, "filtering") {
                    update_undo(UndoType::CoupleEnd);
                    statusline(MessageType::Remark, "Buffer has been filtered");
                } else {
                    statusline(MessageType::Alert, "Could not apply filter output");
                }
            }
        }

        // If there was an error, undo and discard what the command did.
        let last_msg = state().lastmessage;
        if output_was_ingrafted && last_msg == MessageType::Alert {
            do_undo();
            let current_undo = with_state(|s| {
                s.openfile.as_ref().map(|of| of.current_undo).unwrap_or(std::ptr::null_mut())
            });
            discard_until(current_undo);
        }
    }
    #[cfg(not(unix))]
    {
        statusline(MessageType::Alert, "Command execution not supported on this platform");
    }
}

// ---------------------------------------------------------------------------
// insert_a_file_or — insert a file or execute a command
// C: void insert_a_file_or(bool execute)
// ---------------------------------------------------------------------------
pub fn insert_a_file_or(execute: bool) {
    let mut execute = execute;
    let mut response: i32;
    let mut given = String::new();

    #[cfg(feature = "multibuffer")]
    let was_multibuffer = ISSET!(MULTIBUFFER);

    state_mut().as_an_at = false;
    state_mut().ran_a_tool = false;

    #[cfg(not(feature = "tiny"))]
    {
        if execute {
            let foretext = state().foretext.clone().unwrap_or_default();
            if !foretext.is_empty() {
                given = foretext;
            }
        }
    }

    loop {
        let msg: &str;

        #[cfg(not(feature = "tiny"))]
        if execute {
            #[cfg(feature = "multibuffer")]
            if ISSET!(MULTIBUFFER) {
                msg = "Command to execute in new buffer";
            } else {
                msg = "Command to execute";
            }
            #[cfg(not(feature = "multibuffer"))]
            { msg = "Command to execute"; }
        } else {
            #[cfg(feature = "multibuffer")]
            if ISSET!(MULTIBUFFER) {
                #[cfg(not(feature = "tiny"))]
                if ISSET!(NO_CONVERT) {
                    msg = "File to read unconverted into new buffer [from %s]";
                } else {
                    msg = "File to read into new buffer [from %s]";
                }
                #[cfg(feature = "tiny")]
                { msg = "File to read into new buffer [from %s]"; }
            } else {
                #[cfg(not(feature = "tiny"))]
                if ISSET!(NO_CONVERT) {
                    msg = "File to insert unconverted [from %s]";
                } else {
                    msg = "File to insert [from %s]";
                }
                #[cfg(feature = "tiny")]
                { msg = "File to insert [from %s]"; }
            }
            #[cfg(not(feature = "multibuffer"))]
            {
                #[cfg(not(feature = "tiny"))]
                if ISSET!(NO_CONVERT) {
                    msg = "File to insert unconverted [from %s]";
                } else {
                    msg = "File to insert [from %s]";
                }
                #[cfg(feature = "tiny")]
                { msg = "File to insert [from %s]"; }
            }
        }
        #[cfg(feature = "tiny")]
        { msg = "File to insert [from %s]"; }

        state_mut().present_path = Some("./".to_string());

        let menu = if execute { MEXECUTE } else { MINSERTFILE };
        let operating_dir_str = with_state(|s| {
            #[cfg(feature = "operatingdir")]
            { s.operating_dir.clone() }
            #[cfg(not(feature = "operatingdir"))]
            { None::<String> }
        });

        let prompt_default = operating_dir_str.as_deref().unwrap_or("./");
        response = do_prompt(
            menu,
            &given,
            if execute { Some(crate::history::HistoryKind::Execute) } else { None },
            edit_refresh,
            msg,
            prompt_default,
        );

        let ran = state().ran_a_tool;
        let multibuf = ISSET!(MULTIBUFFER);

        if response == -1 || (response == -2 && !multibuf) {
            statusbar("Cancelled");
            break;
        }

        let was_lineno = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .map(|c| c.borrow().lineno)
                .unwrap_or(0)
        });
        let was_x = with_state(|s| {
            s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0)
        });

        let answer = state().answer.clone();
        given = answer.clone();

        if ran {
            break;
        }

        let function = func_from_key(response);

        // Handle function keys
        #[cfg(feature = "multibuffer")]
        if function == Some(crate::global::flip_newbuffer as FuncPtr) {
            if !ISSET!(VIEW_MODE) {
                TOGGLE!(MULTIBUFFER);
            } else {
                beep();
            }
            continue;
        }

        #[cfg(not(feature = "tiny"))]
        {
            if function == Some(crate::global::flip_convert as FuncPtr) {
                TOGGLE!(NO_CONVERT);
                continue;
            }
            if function == Some(crate::global::flip_execute as FuncPtr) {
                execute = !execute;
                continue;
            }
            if function == Some(crate::global::flip_pipe as FuncPtr) {
                add_or_remove_pipe_symbol_from_answer();
                let new_answer = state().answer.clone();
                given = new_answer;
                continue;
            }
        }

        #[cfg(feature = "browser")]
        {
            if function == Some(crate::global::to_files as FuncPtr) {
                if let Some(chosen) = browse_in(&answer) {
                    state_mut().answer = chosen.clone();
                    response = 0;
                } else {
                    continue;
                }
            }
        }

        // If we don't have a file yet, go back to the prompt.
        let multibuf2 = ISSET!(MULTIBUFFER);
        if response != 0 && (!multibuf2 || response != -2) {
            continue;
        }

        let final_answer = state().answer.clone();

        #[cfg(not(feature = "tiny"))]
        if execute {
            #[cfg(feature = "multibuffer")]
            if ISSET!(MULTIBUFFER) {
                open_buffer_impl("", true);
            }

            if !final_answer.is_empty() {
                execute_command(&final_answer);
                #[cfg(feature = "histories")]
                {
                    let mut exec_hist_local = state().execute_history.clone();
                    update_history(&mut exec_hist_local, &final_answer, PRUNE_DUPLICATE);
                    state_mut().execute_history = exec_hist_local;
                }
            }

            #[cfg(feature = "multibuffer")]
            if ISSET!(MULTIBUFFER) {
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        let filetop = of.filetop.clone();
                        of.current = filetop;
                        of.current_x = 0;
                        of.placewewant = 0;
                    }
                });
                set_modified();
            }
        } else {
            open_buffer_impl(&final_answer, ISSET!(MULTIBUFFER));
        }

        #[cfg(feature = "multibuffer")]
        if ISSET!(MULTIBUFFER) {
            #[cfg(feature = "histories")]
            if ISSET!(POSITIONLOG) {
                #[cfg(not(feature = "tiny"))]
                if !execute {
                    restore_cursor_position_if_any();
                }
                #[cfg(feature = "tiny")]
                restore_cursor_position_if_any();
            }
            prepare_for_display();
        } else {
            let cur_lineno = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .map(|c| c.borrow().lineno)
                    .unwrap_or(0)
            });
            let cur_x = with_state(|s| {
                s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0)
            });
            if cur_lineno != was_lineno || cur_x != was_x {
                set_modified();
            }
            state_mut().refresh_needed = true;
        }

        break;
    }

    #[cfg(feature = "multibuffer")]
    if was_multibuffer { SET!(MULTIBUFFER); } else { UNSET!(MULTIBUFFER); }
}

/* C: void do_insertfile(void)
 * If the current mode of operation allows it, go insert a file. */
pub fn do_insertfile() {
    if !in_restricted_mode() {
        insert_a_file_or(false);
    }
}

/* C: void do_execute(void)
 * If the current mode of operation allows it, go prompt for a command. */
#[cfg(not(feature = "tiny"))]
pub fn do_execute() {
    if !in_restricted_mode() {
        insert_a_file_or(true);
    }
}

// ---------------------------------------------------------------------------
// get_full_path — return the canonical absolute path
// C: char *get_full_path(const char *origpath)
// ---------------------------------------------------------------------------
pub fn get_full_path_buf(origpath: &Path) -> Option<PathBuf> {
    if origpath.as_os_str().is_empty() {
        return None;
    }

    let untilded = expand_leading_tilde_path(origpath);
    let path = untilded.as_path();

    #[cfg(feature = "operatingdir")]
    if let Some(result) = with_operating_root(path, |root, relative| {
        let resolved = match root.dir.canonicalize(relative) {
            Ok(resolved) => resolved,
            Err(_) => {
                let filename = relative.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "path has no final component")
                })?;
                let parent = root.dir.canonicalize(usable_parent(relative))?;
                parent.join(filename)
            }
        };
        Ok(root.display_path.join(resolved))
    }) {
        return result.ok();
    }

    if operating_root_required_but_unavailable() {
        return None;
    }

    // Try canonicalize
    match std::fs::canonicalize(path) {
        Ok(canonical) => Some(canonical),
        Err(_) => {
            // Try without the last component (file may not exist yet)
            let parent = usable_parent(path);
            let filename = path.file_name()?;

            if filename.is_empty() {
                return None;
            }

            match std::fs::canonicalize(parent) {
                Ok(canonical_parent) => {
                    Some(canonical_parent.join(filename))
                }
                Err(_) => None,
            }
        }
    }
}

pub fn get_full_path(origpath: &str) -> Option<String> {
    let full = get_full_path_buf(Path::new(origpath))?;
    let mut display = printable_path(&full);
    if full.is_dir()
        && full.parent().is_some()
        && !display.ends_with(std::path::MAIN_SEPARATOR)
    {
        display.push(std::path::MAIN_SEPARATOR);
    }
    Some(display)
}

// ---------------------------------------------------------------------------
// check_writable_directory — verify path is a writable directory
// C: char *check_writable_directory(const char *path)
// ---------------------------------------------------------------------------
pub fn check_writable_directory(path: &str) -> Option<String> {
    let full_path = get_full_path(path)?;

    if !full_path.ends_with('/') {
        return None;
    }

    let cpath = std::ffi::CString::new(full_path.as_str()).ok()?;
    #[cfg(unix)]
    {
        let ret = unsafe { libc::access(cpath.as_ptr(), libc::W_OK) };
        if ret != 0 {
            return None;
        }
    }

    Some(full_path)
}

// ---------------------------------------------------------------------------
// safe_tempfile — create a temporary file
// C: char *safe_tempfile(FILE **stream)
// Returns (path, File) on success, None on failure.
// ---------------------------------------------------------------------------
fn temporary_suffix(filename: &str) -> String {
    Path::new(filename)
        .extension()
        .filter(|extension| !extension.is_empty())
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default()
}

pub fn safe_tempfile() -> Option<(String, File)> {
    // std::env::temp_dir() honors the platform-native temp location (including
    // the Windows APIs/environment contract) instead of hard-coding /tmp.
    let tempdir = std::env::temp_dir();

    let extension = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|of| temporary_suffix(&of.filename))
            .unwrap_or_default()
    });

    // Use tempfile crate (cross-platform)
    let temp_builder = tempfile::Builder::new()
        .prefix("nano.")
        .suffix(&extension)
        .tempfile_in(&tempdir);

    match temp_builder {
        Ok(temp_file) => {
            let (file, path) = temp_file.keep().ok()?;
            let tempfile_name = path.to_string_lossy().into_owned();
            Some((tempfile_name, file))
        }
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// init_operating_dir — change to the operating directory
// C: void init_operating_dir(void)
// ---------------------------------------------------------------------------
#[cfg(feature = "operatingdir")]
pub fn init_operating_dir() {
    let od = state().operating_dir.clone().unwrap_or_default();
    let target = get_full_path(&od);

    match target {
        None => {
            eprintln!("Invalid operating directory: {}", od);
            std::process::exit(1);
        }
        Some(t) => {
            if std::env::set_current_dir(&t).is_err() {
                eprintln!("Invalid operating directory: {}", od);
                std::process::exit(1);
            }
            // The process cwd itself identifies the directory selected by
            // set_current_dir even if its old name is immediately replaced.
            // Retain that exact object as the capability root, rather than
            // reopening the attacker-mutable pathname a second time.
            let directory = match cap_std::fs::Dir::open_ambient_dir(
                ".",
                cap_std::ambient_authority(),
            ) {
                Ok(directory) => directory,
                Err(_) => {
                    eprintln!("Invalid operating directory: {}", od);
                    std::process::exit(1);
                }
            };
            let display_path = PathBuf::from(&t);
            OPERATING_ROOT.with(|slot| {
                *slot.borrow_mut() = Some(OperatingRoot {
                    display_path,
                    dir: directory,
                });
            });
            state_mut().operating_dir = Some(t);
        }
    }
}

/* C: bool outside_of_confinement(const char *somepath, bool tabbing)
 * Check whether the given path is outside of the operating directory. */
#[cfg(feature = "operatingdir")]
pub fn outside_of_confinement_path(somepath: &Path, tabbing: bool) -> bool {
    let expanded = expand_leading_tilde_path(somepath);
    let path = expanded.as_path();
    if let Some(result) = with_operating_root(path, |root, relative| {
        // Canonicalization in cap-std is rooted at the retained handle and
        // rejects symlinks or `..` components that leave it.  A missing final
        // component is allowed when its actual parent is still beneath root.
        if root.dir.canonicalize(relative).is_ok() {
            return Ok(true);
        }
        let parent = usable_parent(relative);
        Ok(root.dir.canonicalize(parent).is_ok())
    }) {
        return !result.unwrap_or(false);
    }

    if tabbing {
        let operating_dir = state().operating_dir_raw.clone().unwrap_or_default();
        return !operating_dir.starts_with(path);
    }
    true
}

#[cfg(feature = "operatingdir")]
pub fn outside_of_confinement(somepath: &str, tabbing: bool) -> bool {
    outside_of_confinement_path(Path::new(somepath), tabbing)
}

#[cfg(not(feature = "operatingdir"))]
pub fn outside_of_confinement(_somepath: &str, _tabbing: bool) -> bool {
    false
}

#[cfg(not(feature = "operatingdir"))]
pub fn outside_of_confinement_path(_somepath: &Path, _tabbing: bool) -> bool {
    false
}

// ---------------------------------------------------------------------------
// init_backup_dir (!NANO_TINY)
// C: void init_backup_dir(void)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "tiny"))]
pub fn init_backup_dir() {
    let bd = state().backup_dir.clone().unwrap_or_default();
    let target = get_full_path(&bd);

    match target {
        None => {
            eprintln!("Invalid backup directory: {}", bd);
            std::process::exit(1);
        }
        Some(t) => {
            if !t.ends_with('/') {
                eprintln!("Invalid backup directory: {}", bd);
                std::process::exit(1);
            }
            state_mut().backup_dir = Some(t);
        }
    }
}

// ---------------------------------------------------------------------------
// copy_file — copy all data from `inn` to `out`
// C: int copy_file(FILE *inn, FILE *out, bool close_out)
// Returns 0 on success, negative on read error, positive on write error.
// ---------------------------------------------------------------------------
pub fn copy_file(mut inn: File, mut out: File, close_out: bool) -> i32 {
    let mut buf = [0u8; 8192];
    let mut retval = 0i32;

    loop {
        let n = match inn.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => { retval = -1; break; }
        };
        match out.write_all(&buf[..n]) {
            Ok(_) => {}
            Err(_) => { retval = 2; break; }
        }
    }

    // Always close inn
    drop(inn);

    if retval != 0 {
        if !close_out { let _ = out.flush(); }
        drop(out);
        return retval;
    }

    if close_out {
        if out.flush().is_err() { retval = 4; }
        drop(out);
    } else {
        if out.flush().is_err() { retval = 4; }
    }

    retval
}

// ---------------------------------------------------------------------------
// make_backup_of (!NANO_TINY) — create a backup copy of the file
// C: bool make_backup_of(char *realname, struct stat fileinfo)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "tiny"))]
fn backup_path_key(path: &Path) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::from("p");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        for byte in path.as_os_str().as_bytes() {
            let _ = write!(encoded, "{:02X}", byte);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            let _ = write!(encoded, "{:04X}", unit);
        }
    }
    #[cfg(not(any(unix, windows)))]
    for byte in path.to_string_lossy().as_bytes() {
        let _ = write!(encoded, "{:02X}", byte);
    }
    encoded
}

#[cfg(not(feature = "tiny"))]
pub fn make_backup_of(realname: &Path, fileinfo: &FileStat) -> bool {
    statusbar("Making backup...");

    let backup_dir = state().backup_dir.clone();

    let backupname: PathBuf = if backup_dir.is_none() {
        // Byte-preserving "<name>~" so a non-UTF-8 file backs up beside itself.
        let mut name = realname.as_os_str().to_os_string();
        name.push("~");
        PathBuf::from(name)
    } else {
        let bd = backup_dir.as_ref().unwrap();
        let source_path = get_full_path_buf(realname)
            .unwrap_or_else(|| realname.to_path_buf());
        // backup_path_key hex-encodes the path bytes, so the result is plain
        // ASCII and safe to handle as a string.
        let thename = backup_path_key(&source_path);
        let base = Path::new(bd).join(thename).to_string_lossy().into_owned();
        let next = get_next_filename(&base, "~");
        if next.is_empty() {
            statusline(MessageType::Alert, "Too many existing backup files");
            return false;
        }
        PathBuf::from(next)
    };

    let fail = |reason: &str| {
        warn_and_briefly_pause("Cannot make backup");
        warn_and_briefly_pause(reason);
        if ask_user(YESORNO, "Cannot make backup; continue and save actual file? ") == YES {
            true
        } else {
            statusline(MessageType::Hush, &format!("Cannot make backup: {}", reason));
            false
        }
    };

    let mut original = match open_path(realname) {
        Ok(file) => file,
        Err(error) => return fail(&format!("Cannot read original file: {}", error)),
    };

    // Stage beside the destination so the final rename is atomic.  tempfile
    // creates this path exclusively with mode 0600, and its Drop removes any
    // incomplete candidate without disturbing an older complete backup.
    let parent = usable_parent(&backupname);
    let mut staging = match create_staging_file(parent, ".nano-backup.") {
        Ok(file) => file,
        Err(error) => return fail(&error.to_string()),
    };

    if let Err(error) = io::copy(&mut original, staging.as_file_mut()) {
        return fail(&format!("Cannot copy original file: {}", error));
    }

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = staging.as_file().as_raw_fd();
        let ownership = unsafe { libc::fchown(fd, fileinfo.st_uid, fileinfo.st_gid) };
        if ownership != 0 {
            return fail(&format!("Cannot preserve backup ownership: {}", io::Error::last_os_error()));
        }
        let permissions = unsafe { libc::fchmod(fd, fileinfo.st_mode & 0o7777) };
        if permissions != 0 {
            return fail(&format!("Cannot preserve backup permissions: {}", io::Error::last_os_error()));
        }

        let times = [
            libc::timespec { tv_sec: fileinfo.st_atime, tv_nsec: fileinfo.st_atime_nsec },
            libc::timespec { tv_sec: fileinfo.st_mtime, tv_nsec: fileinfo.st_mtime_nsec },
        ];
        if unsafe { libc::futimens(fd, times.as_ptr()) } != 0 {
            return fail(&format!("Cannot preserve backup timestamps: {}", io::Error::last_os_error()));
        }
    }

    if let Err(error) = staging.as_file_mut().flush() {
        return fail(&format!("Cannot flush backup: {}", error));
    }
    if let Err(error) = staging.as_file().sync_all() {
        return fail(&format!("Cannot sync backup: {}", error));
    }

    let persisted = match staging.persist(&backupname) {
        Ok(file) => file,
        Err(error) => return fail(&format!("Cannot install backup: {}", error)),
    };
    drop(persisted);

    #[cfg(unix)]
    if let Err(error) = sync_parent_of(&backupname) {
        return fail(&format!("Cannot sync backup directory: {}", error));
    }

    true
}

// ---------------------------------------------------------------------------
// write_file — write the current buffer to disk
// C: bool write_file(const char *name, FILE *thefile, bool normal,
//                    kind_of_writing_type method, bool annotate)
// ---------------------------------------------------------------------------
pub fn write_file(
    name: impl AsRef<Path>,
    thefile: Option<File>,
    normal: bool,
    method: KindOfWritingType,
    annotate: bool,
) -> bool {
    // The authoritative path; `realname_str` exists only for messages.
    let realname: PathBuf = expand_leading_tilde_path(name.as_ref());
    let realname_str = printable_path(&realname);

    #[cfg(feature = "operatingdir")]
    if normal {
        let confined = with_state(|s| {
            s.operating_dir.as_deref()
                .map(|_od| outside_of_confinement_path(&realname, false))
                .unwrap_or(false)
        });
        if confined {
            let od = state().operating_dir.clone().unwrap_or_default();
            statusline(MessageType::Alert, &format!("Can't write outside of {}", od));
            return false;
        }
    }

    if normal {
        let has_lossy_data = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|of| of.had_invalid_utf8)
                .unwrap_or(false)
        });
        if has_lossy_data {
            statusline(
                MessageType::Alert,
                "Cannot safely save: buffer contains invalid UTF-8 bytes",
            );
            return false;
        }
    }

    #[cfg(not(feature = "tiny"))]
    let mut prepend_staging: Option<StagingFile> = None;
    #[cfg(not(feature = "tiny"))]
    let mut prepend_source: Option<File> = None;
    #[cfg(not(feature = "tiny"))]
    let mut prepend_permissions: Option<std::fs::Permissions> = None;
    #[cfg(all(not(feature = "tiny"), unix))]
    let mut prepend_statinfo: Option<FileStat> = None;
    let mut lineswritten: usize = 0;

    #[cfg(not(feature = "tiny"))]
    let is_existing_file = {
        normal && path_info(&realname).is_ok()
    };

    // Make backup if needed
    #[cfg(not(feature = "tiny"))]
    if ISSET!(MAKE_BACKUP) && is_existing_file {
        // Check it's not a FIFO
        let is_fifo = path_info(&realname)
            .map(|info| info.is_fifo)
            .unwrap_or(false);
        if !is_fifo {
            if let Some(st) = stat_with_alloc(&realname) {
                if !make_backup_of(&realname, &st) {
                    return false;
                }
            }
        }
    }

    // Build a prepend result in a same-directory staging file.  The live
    // destination is not opened for writing until the complete candidate has
    // been flushed, synced, and atomically installed.
    #[cfg(not(feature = "tiny"))]
    if method == KindOfWritingType::Prepend {
        let is_fifo = path_info(&realname)
            .map(|info| info.is_fifo)
            .unwrap_or(false);
        if is_fifo {
            statusline(MessageType::Alert, &format!("Error writing {}: FIFO", realname_str));
            return false;
        }

        let source = match open_path(&realname) {
            Err(e) => {
                statusline(MessageType::Alert,
                    &format!("Error reading {}: {}", realname_str, e));
                return false;
            }
            Ok(f) => f,
        };
        prepend_permissions = source.metadata().ok().map(|metadata| metadata.permissions());
        #[cfg(unix)]
        {
            prepend_statinfo = stat_with_alloc(&realname);
        }

        let parent = usable_parent(&realname);
        prepend_staging = match create_staging_file(parent, ".nano-prepend.") {
            Ok(file) => Some(file),
            Err(error) => {
                statusline(MessageType::Alert, &format!("Error creating prepend staging file: {}", error));
                return false;
            }
        };
        prepend_source = Some(source);
    }

    #[cfg(not(feature = "tiny"))]
    {
        let is_fifo = path_info(&realname)
            .map(|info| info.is_fifo)
            .unwrap_or(false);
        if is_existing_file && is_fifo {
            statusbar("Writing to FIFO...");
        }
    }

    // Open / create the file when not writing to a temp file
    #[cfg(not(feature = "tiny"))]
    let staged_output = match prepend_staging.as_ref() {
        Some(staging) => match staging.as_file().try_clone() {
            Ok(file) => Some(file),
            Err(error) => {
                statusline(MessageType::Alert, &format!("Error opening prepend staging file: {}", error));
                return false;
            }
        },
        None => None,
    };
    #[cfg(feature = "tiny")]
    let staged_output: Option<File> = None;

    let mut the_file: File = match staged_output.or(thefile) {
        Some(f) => f,
        None => {
            let permissions: u32 = if normal { 0o666 } else { 0o600 };

            #[cfg(not(feature = "tiny"))]
            block_sigwinch(true);
            #[cfg(not(feature = "tiny"))]
            if normal { install_handler_for_Ctrl_C(); }

            let open_result = match method {
                KindOfWritingType::Append => open_path_with(
                    &realname,
                    PathOpenOptions {
                        write: true,
                        create: true,
                        append: true,
                        mode: permissions,
                        ..PathOpenOptions::default()
                    },
                ),
                KindOfWritingType::Emergency => open_path_with(
                    &realname,
                    PathOpenOptions {
                        write: true,
                        create_new: true,
                        mode: permissions,
                        ..PathOpenOptions::default()
                    },
                ),
                _ => open_path_with(
                    &realname,
                    PathOpenOptions {
                        write: true,
                        create: true,
                        truncate: true,
                        mode: permissions,
                        ..PathOpenOptions::default()
                    },
                ),
            };

            #[cfg(not(feature = "tiny"))]
            if normal { restore_handler_for_Ctrl_C(); }
            #[cfg(not(feature = "tiny"))]
            block_sigwinch(false);

            match open_result {
                Err(e) => {
                    let kind = e.kind();
                    if kind == io::ErrorKind::Interrupted {
                        statusline(MessageType::Alert, "Interrupted");
                    } else {
                        statusline(MessageType::Alert,
                            &format!("Error writing {}: {}", realname_str, e));
                    }
                    return false;
                }
                Ok(f) => f,
            }
        }
    };

    if normal {
        statusbar("Writing...");
    }

    // Buffer the output. A raw File issues a syscall per write_all (2-3 per line),
    // which dominates write time on large files; BufWriter coalesces them into
    // ~8KB flushes, mirroring C's fdopen+fwrite buffered stdio (files.c:1818).
    // It borrows `the_file`; the borrow is released (drop(writer)) before the
    // sync_all below, and write-error paths just `return false` (the File closes
    // at scope end, matching C's fclose-then-discard).
    let mut writer = std::io::BufWriter::new(&the_file);

    // The line ending (DOS vs Unix) is loop-invariant — fetch it once.
    #[cfg(not(feature = "tiny"))]
    let fmt = with_state(|s| {
        s.openfile.as_ref().map(|of| of.fmt).unwrap_or(FormatType::Unspecified)
    });

    // Write the buffer line by line
    let filetop = with_state(|s| s.openfile.as_ref().and_then(|of| of.filetop.clone()));
    let mut line = filetop;

    loop {
        // Write this line's data inside the node borrow (no clone), then capture
        // what's needed to advance. Recode LF->NUL only when the line actually
        // contains an embedded LF (i.e. it held a NUL on read) — the overwhelming
        // majority of lines take the borrow-and-write-direct fast path, mirroring
        // the read-side no-NUL fast path in encode_data (files.rs:1077).
        let (data_res, has_next, data_empty, next) = {
            let node = match &line {
                None => break,
                Some(n) => n,
            };
            let nb = node.borrow();
            let bytes = nb.data.as_bytes();
            let res = if bytes.contains(&b'\n') {
                let recoded: Vec<u8> =
                    bytes.iter().map(|&b| if b == b'\n' { 0 } else { b }).collect();
                writer.write_all(&recoded)
            } else {
                writer.write_all(bytes)
            };
            (res, nb.next.is_some(), nb.data.is_empty(), nb.next.clone())
        };

        if data_res.is_err() {
            let e = io::Error::last_os_error();
            statusline(MessageType::Alert, &format!("Error writing {}: {}", realname_str, e));
            return false;
        }

        // If we've reached the last line, don't write a trailing newline.
        if !has_next {
            if !data_empty {
                lineswritten += 1;
            }
            break;
        }

        // Write newline (preceded by CR for DOS format)
        #[cfg(not(feature = "tiny"))]
        if fmt == FormatType::DosFile {
            if writer.write_all(b"\r").is_err() {
                let e = io::Error::last_os_error();
                statusline(MessageType::Alert, &format!("Error writing {}: {}", realname_str, e));
                return false;
            }
        }

        if writer.write_all(b"\n").is_err() {
            let e = io::Error::last_os_error();
            statusline(MessageType::Alert, &format!("Error writing {}: {}", realname_str, e));
            return false;
        }

        lineswritten += 1;
        line = next;
    }

    // When prepending, append the still-untouched original to the staged new
    // content.  Any failure drops the staging file and preserves the target.
    #[cfg(not(feature = "tiny"))]
    if method == KindOfWritingType::Prepend {
        if let Some(mut source) = prepend_source.take() {
            let mut buf = [0u8; 8192];
            loop {
                let n = match source.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        statusline(MessageType::Alert,
                            &format!("Error reading {}: {}", realname_str, e));
                        return false;
                    }
                };
                if writer.write_all(&buf[..n]).is_err() {
                    let e = io::Error::last_os_error();
                    statusline(MessageType::Alert,
                        &format!("Error writing {}: {}", realname_str, e));
                    return false;
                }
            }
        }
    }

    // Flush the buffered writer and release its borrow on `the_file` before the
    // durability sync below (sync_all is a File method, not on the BufWriter).
    if writer.flush().is_err() {
        let e = io::Error::last_os_error();
        statusline(MessageType::Alert, &format!("Error writing {}: {}", realname_str, e));
        return false;
    }
    drop(writer);

    // Flush and sync (not for FIFOs)
    #[cfg(not(feature = "tiny"))]
    {
        let is_fifo = path_info(&realname)
            .map(|info| info.is_fifo)
            .unwrap_or(false);
        if !is_fifo {
            if the_file.flush().is_err() || the_file.sync_all().is_err() {
                let e = io::Error::last_os_error();
                statusline(MessageType::Alert, &format!("Error writing {}: {}", realname_str, e));
                drop(the_file);
                return false;
            }
        }
    }

    // Close the file
    if the_file.flush().is_err() {
        let e = io::Error::last_os_error();
        statusline(MessageType::Alert, &format!("Error writing {}: {}", realname_str, e));

        // Check for ENOSPC
        #[cfg(not(feature = "tiny"))]
        {
            #[cfg(unix)]
            let is_enospc = e.raw_os_error() == Some(libc::ENOSPC);
            #[cfg(not(unix))]
            let is_enospc = false;

            if is_enospc && normal {
                napms(3200);
                state_mut().lastmessage = MessageType::Vacuum;
                statusline(MessageType::Alert, "File on disk has been truncated!");
                napms(3200);
                state_mut().lastmessage = MessageType::Vacuum;
                statusline(MessageType::Alert,
                    "Maybe ^T^Z, make room on disk, resume, then ^S^X");
                let st = stat_with_alloc(&realname);
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.statinfo = st;
                    }
                });
            }
        }

        return false;
    }
    drop(the_file);

    #[cfg(not(feature = "tiny"))]
    if method == KindOfWritingType::Prepend {
        let mut staging = match prepend_staging.take() {
            Some(file) => file,
            None => {
                statusline(MessageType::Alert, "Missing prepend staging file");
                return false;
            }
        };

        #[cfg(unix)]
        if let Some(info) = prepend_statinfo.as_ref() {
            use std::os::unix::io::AsRawFd;
            let fd = staging.as_file().as_raw_fd();
            if unsafe { libc::fchown(fd, info.st_uid, info.st_gid) } != 0 {
                statusline(MessageType::Alert, &format!(
                    "Error preserving ownership of {}: {}",
                    realname_str,
                    io::Error::last_os_error()
                ));
                return false;
            }
            if unsafe { libc::fchmod(fd, info.st_mode & 0o7777) } != 0 {
                statusline(MessageType::Alert, &format!(
                    "Error preserving permissions of {}: {}",
                    realname_str,
                    io::Error::last_os_error()
                ));
                return false;
            }
        }

        #[cfg(not(unix))]
        if let Some(permissions) = prepend_permissions.take() {
            if let Err(error) = staging.as_file().set_permissions(permissions) {
                statusline(MessageType::Alert, &format!(
                    "Error preserving permissions of {}: {}",
                    realname_str, error
                ));
                return false;
            }
        }

        if let Err(error) = staging.as_file_mut().flush().and_then(|_| staging.as_file().sync_all()) {
            statusline(MessageType::Alert, &format!("Error syncing {}: {}", realname_str, error));
            return false;
        }

        match staging.persist(&realname) {
            Ok(file) => drop(file),
            Err(error) => {
                statusline(MessageType::Alert, &format!("Error installing {}: {}", realname_str, error));
                return false;
            }
        }

        #[cfg(unix)]
        if let Err(error) = sync_parent_of(&realname) {
            statusline(MessageType::Alert, &format!("Error syncing directory for {}: {}", realname_str, error));
            return false;
        }
    }

    // When having written an entire buffer, update administrivia.
    if annotate && method == KindOfWritingType::Overwrite {
        // Compare against the authoritative path; fall back to the display
        // string only for buffers that never had a real path recorded.
        let name_changed = with_state(|s| {
            s.openfile.as_ref().map(|of| {
                match openfile_filename_path(of) {
                    Some(old_path) => old_path != realname,
                    None => of.filename != realname_str,
                }
            }).unwrap_or(true)
        });

        if name_changed {
            #[cfg(not(feature = "tiny"))]
            {
                let (lock_file, lock_fname, lock_path) = with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        (of.lock_file.take(), of.lock_filename.take(), of.lock_path.take())
                    } else {
                        (None, None, None)
                    }
                });
                if let Some(lp) = lock_path {
                    delete_lockfile(&lp, lock_file.as_ref());
                } else if let Some(lf) = lock_fname {
                    delete_lockfile(&lf, lock_file.as_ref());
                }
                drop(lock_file);
                if ISSET!(LOCKING) {
                    let lock_result = do_lockfile(&realname, false);
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            if let Some((lock_path, lock_file)) = lock_result.ok().flatten() {
                                of.lock_filename = Some(printable_path(&lock_path));
                                of.lock_path = Some(lock_path);
                                of.lock_file = Some(lock_file);
                            }
                        }
                    });
                }
            }

            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    set_openfile_filename(of, realname.clone());
                }
            });

            #[cfg(feature = "color")]
            {
                let was_syntax = with_state(|s| {
                    s.openfile.as_ref().and_then(|of| of.syntax).map(|p| p as usize)
                });
                find_and_prime_applicable_syntax();
                let new_syntax = with_state(|s| {
                    s.openfile.as_ref().and_then(|of| of.syntax).map(|p| p as usize)
                });
                if was_syntax != new_syntax {
                    // Clear multidata and recompute
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            let mut ln = of.filetop.clone();
                            while let Some(node) = ln {
                                node.borrow_mut().multidata.clear();
                                let next = node.borrow().next.clone();
                                ln = next;
                            }
                        }
                        s.have_palette = false;
                        s.refresh_needed = true;
                    });
                    precalc_multicolorinfo();
                }
            }
        }

        #[cfg(not(feature = "tiny"))]
        {
            let st = stat_with_alloc(&realname);
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.statinfo = st;
                    of.last_saved = of.current_undo;
                    of.last_action = UndoType::Other;
                }
            });
        }

        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.modified = false;
            }
        });
        titlebar(None);
    }

    // Report the number of lines written
    #[cfg(not(feature = "tiny"))]
    {
        let minibar = ISSET!(MINIBAR);
        let zero = ISSET!(ZERO);
        let lines = LINES();
        if minibar && !zero && lines > 1 && annotate {
            state_mut().report_size = true;
            return true;
        }
    }

    if normal {
        let msg = if lineswritten == 1 {
            format!("Wrote {} line", lineswritten)
        } else {
            format!("Wrote {} lines", lineswritten)
        };
        statusline(MessageType::Remark, &msg);
    }

    true
}

// ---------------------------------------------------------------------------
// write_region_to_file (!NANO_TINY)
// C: bool write_region_to_file(const char *name, FILE *stream, bool normal,
//                               kind_of_writing_type method)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "tiny"))]
pub fn write_region_to_file(
    name: impl AsRef<Path>,
    stream: Option<File>,
    normal: bool,
    method: KindOfWritingType,
) -> bool {
    let mut topline: Option<LinePtr> = None;
    let mut botline: Option<LinePtr> = None;
    let mut top_x: usize = 0;
    let mut bot_x: usize = 0;

    get_region(&mut topline, &mut top_x, &mut botline, &mut bot_x);

    // When needed, prepare a magic end line
    let stopper: Option<LinePtr> = if normal && bot_x > 0 && !ISSET!(NO_NEWLINES) {
        let new_node = make_new_node(botline.clone());
        new_node.borrow_mut().data = String::new();
        if let Some(ref bot) = botline {
            bot.borrow_mut().next = Some(new_node.clone());
        }
        Some(new_node)
    } else {
        None
    };

    // Make the marked area look like a separate buffer.  As in copy_marked_region
    // this must be NON-destructive: C writes a single '\0' at bot_x and bumps the
    // topline data pointer past top_x, then restores the exact original strings.
    // These are LIVE document nodes, so save their full data before mutating.
    let birthline = with_state(|s| s.openfile.as_ref().and_then(|of| of.filetop.clone()));
    let after_line = botline.as_ref().and_then(|b| b.borrow().next.clone());

    let saved_top_data = topline.as_ref().map(|t| t.borrow().data.clone());
    let saved_bot_data = botline.as_ref().map(|b| b.borrow().data.clone());

    // Logically truncate botline at bot_x and attach the magic stopper (if any).
    if let Some(ref bot) = botline {
        let mut b = bot.borrow_mut();
        if bot_x <= b.data.len() && b.data.is_char_boundary(bot_x) {
            b.data.truncate(bot_x);
        }
        b.next = stopper.clone();
    }

    // Drop topline's prefix before top_x.  When topline == botline this runs after
    // the truncate above, yielding data[top_x..bot_x] — exactly C's behaviour.
    if let Some(ref top) = topline {
        let mut t = top.borrow_mut();
        if top_x <= t.data.len() && t.data.is_char_boundary(top_x) {
            let moved = t.data[top_x..].to_string();
            t.data = moved;
        }
    }

    // Set filetop to topline for the duration of the write.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.filetop = topline.clone();
        }
    });

    let retval = write_file(name, stream, normal, method, NONOTES);

    // Restore the proper state of the buffer.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.filetop = birthline;
        }
    });

    if let Some(ref top) = topline {
        if let Some(data) = saved_top_data {
            top.borrow_mut().data = data;
        }
    }
    if let Some(ref bot) = botline {
        let mut b = bot.borrow_mut();
        if let Some(data) = saved_bot_data {
            b.data = data;
        }
        b.next = after_line;
    }

    if let Some(stopper_node) = stopper {
        delete_node(stopper_node);
    }

    retval
}

// ---------------------------------------------------------------------------
// write_it_out — write current buffer (or marked region) to disk
// C: int write_it_out(bool exiting, bool withprompt)
// Returns 0 on error, 1 on success, 2 when buffer is to be discarded.
// ---------------------------------------------------------------------------
pub fn write_it_out(exiting: bool, withprompt: bool) -> i32 {
    let given = {
        #[cfg(not(feature = "tiny"))]
        {
            let mark_on = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
            });
            if mark_on && !exiting {
                String::new()
            } else {
                with_state(|s| s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default())
            }
        }
        #[cfg(feature = "tiny")]
        {
            with_state(|s| s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default())
        }
    };

    let mut given = given;
    let maychange_initial = with_state(|s| {
        s.openfile.as_ref().map(|of| of.filename.is_empty()).unwrap_or(true)
    });
    let mut maychange = maychange_initial;
    let mut method = KindOfWritingType::Overwrite;

    state_mut().as_an_at = false;

    #[cfg(feature = "extra")]
    let mut did_credits = false;

    loop {
        let response: i32;
        let choice;

        #[cfg(not(feature = "tiny"))]
        let formatstr = {
            let fmt = with_state(|s| {
                s.openfile.as_ref().map(|of| of.fmt).unwrap_or(FormatType::Unspecified)
            });
            if fmt == FormatType::DosFile { " [DOS Format]" } else { "" }
        };
        #[cfg(feature = "tiny")]
        let formatstr = "";

        #[cfg(not(feature = "tiny"))]
        let backupstr = if ISSET!(MAKE_BACKUP) { " [Backup]" } else { "" };
        #[cfg(feature = "tiny")]
        let backupstr = "";

        let msg = {
            #[cfg(not(feature = "tiny"))]
            {
                let mark_on = with_state(|s| {
                    s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
                });
                let restricted = ISSET!(RESTRICTED);
                if mark_on && !exiting && !restricted {
                    match method {
                        KindOfWritingType::Prepend => "Prepend Selection to File",
                        KindOfWritingType::Append  => "Append Selection to File",
                        _                          => "Write Selection to File",
                    }
                } else if method != KindOfWritingType::Overwrite {
                    match method {
                        KindOfWritingType::Prepend => "Prepend to File",
                        _                          => "Append to File",
                    }
                } else {
                    "Write to File"
                }
            }
            #[cfg(feature = "tiny")]
            "Write to File"
        };

        state_mut().present_path = Some("./".to_string());

        let save_on_exit = ISSET!(SAVE_ON_EXIT);
        let has_filename = with_state(|s| {
            s.openfile.as_ref().map(|of| !of.filename.is_empty()).unwrap_or(false)
        });

        let skip_prompt = (!withprompt || (save_on_exit && exiting)) && has_filename;

        if skip_prompt {
            let fname = with_state(|s| s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default());
            state_mut().answer = fname;
            response = 0;
        } else {
            let prompt_str = format!("{}{}{}", msg, formatstr, backupstr);
            response = do_prompt(
                MWRITEFILE,
                &given,
                None,
                edit_refresh,
                &prompt_str,
                "",
            );
        }

        if response < 0 {
            statusbar("Cancelled");
            return 0;
        }

        let function = func_from_key(response);

        // Handle discard
        if function == Some(crate::global::discard_buffer as FuncPtr) {
            state_mut().final_status = 2;
            return 2;
        }

        let answer = state().answer.clone();
        given = answer.clone();

        #[cfg(feature = "browser")]
        {
            let restricted = ISSET!(RESTRICTED);
            if function == Some(crate::global::to_files as FuncPtr) && !restricted {
                if let Some(chosen) = browse_in(&answer) {
                    state_mut().answer = chosen;
                } else {
                    continue;
                }
            }
        }

        let answer2 = state().answer.clone();

        #[cfg(not(feature = "tiny"))]
        {
            if function == Some(crate::global::dos_format as FuncPtr) {
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.fmt = if of.fmt == FormatType::DosFile {
                            FormatType::NixFile
                        } else {
                            FormatType::DosFile
                        };
                    }
                });
                continue;
            }
            let restricted = ISSET!(RESTRICTED);
            if function == Some(crate::global::back_it_up as FuncPtr) && !restricted {
                TOGGLE!(MAKE_BACKUP);
                continue;
            }
            if (function == Some(crate::global::prepend_it as FuncPtr)
                || function == Some(crate::global::append_it as FuncPtr))
                && !restricted
            {
                if function == Some(crate::global::prepend_it as FuncPtr) {
                    method = if method == KindOfWritingType::Prepend {
                        KindOfWritingType::Overwrite
                    } else {
                        KindOfWritingType::Prepend
                    };
                } else {
                    method = if method == KindOfWritingType::Append {
                        KindOfWritingType::Overwrite
                    } else {
                        KindOfWritingType::Append
                    };
                }
                let of_fname = with_state(|s| {
                    s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default()
                });
                if answer2 == of_fname {
                    given.clear();
                }
                continue;
            }
        }

        if function == Some(crate::global::do_help as FuncPtr) {
            continue;
        }

        // Easter egg
        #[cfg(feature = "extra")]
        {
            let of_fname = with_state(|s| {
                s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default()
            });
            if exiting && !ISSET!(SAVE_ON_EXIT) && of_fname.is_empty()
                && answer2 == "zzy" && !did_credits
            {
                let lines = LINES();
                let cols = COLS();
                if lines > 5 && cols > 31 {
                    do_credits();
                    // C parity: guards a repeat showing; our port returns
                    // right after, so the value is never read again.
                    #[allow(unused_assignments)]
                    {
                        did_credits = true;
                    }
                } else {
                    statusline(MessageType::Ahem, "Too tiny");
                }
                return 0;
            }
        }

        if method == KindOfWritingType::Overwrite {
            let full_answer = get_full_path(&answer2);
            let full_filename = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| get_full_path(&of.filename))
            });
            let of_filename_empty = with_state(|s| {
                s.openfile.as_ref().map(|of| of.filename.is_empty()).unwrap_or(true)
            });
            let answer_path = full_answer.as_deref().unwrap_or(&answer2);
            let of_filename_string: String = full_filename.clone().unwrap_or_else(|| {
                with_state(|s| s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default())
            });
            let of_path: &str = &of_filename_string;
            let name_exists = path_exists_nofollow(Path::new(answer_path)).unwrap_or(false);
            let do_warning = if of_filename_empty {
                name_exists
            } else {
                answer_path != of_path
            };

            if do_warning {
                let restricted = ISSET!(RESTRICTED);
                if restricted {
                    warn_and_briefly_pause("File exists -- cannot overwrite");
                    continue;
                }

                if !maychange {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let mark_on = with_state(|s| {
                            s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
                        });
                        if exiting || !mark_on {
                            if ask_user(YESORNO, "Save file under DIFFERENT NAME? ") != YES {
                                continue;
                            }
                            maychange = true;
                        }
                    }
                    #[cfg(feature = "tiny")]
                    {
                        if ask_user(YESORNO, "Save file under DIFFERENT NAME? ") != YES {
                            continue;
                        }
                        maychange = true;
                    }
                }

                if name_exists {
                    let question = "File \"%s\" exists; OVERWRITE? ";
                    let room = COLS() as isize - breadth(question) as isize + 1;
                    let name = crop_to_fit(&answer2, room);
                    let message = format!("File \"{}\" exists; OVERWRITE? ", name);
                    choice = ask_user(YESORNO, &message);
                    if choice != YES {
                        continue;
                    }
                }
            } else {
                #[cfg(not(feature = "tiny"))]
                {
                    if name_exists {
                        let (stat_changed, stat_mtime, stat_dev, stat_ino) = with_state(|s| {
                            if let Some(ref of) = s.openfile {
                                if let Some(ref si) = of.statinfo {
                                    (true, si.st_mtime, si.st_dev, si.st_ino)
                                } else {
                                    (false, 0, 0, 0)
                                }
                            } else {
                                (false, 0, 0, 0)
                            }
                        });

                        if stat_changed {
                            let new_st = stat_with_alloc(&answer2);
                            let changed = new_st.as_ref().map(|ns| {
                                ns.st_mtime > stat_mtime
                                || ns.st_dev != stat_dev
                                || ns.st_ino != stat_ino
                            }).unwrap_or(false);

                            if changed {
                                warn_and_briefly_pause("File on disk has changed");
                                choice = ask_user(YESORNO,
                                    "File was modified since you opened it; continue saving? ");
                                wipe_statusbar();

                                if ISSET!(SAVE_ON_EXIT) && withprompt {
                                    // Saving to the buffer's own file: use the
                                    // authoritative path when one is recorded.
                                    let fname = with_state(|s| {
                                        s.openfile.as_ref().map(|of| {
                                            match openfile_filename_path(of) {
                                                Some(path) => path.to_path_buf(),
                                                None => PathBuf::from(&of.filename),
                                            }
                                        }).unwrap_or_default()
                                    });
                                    if choice == YES {
                                        return write_file(&fname, None, NORMAL, KindOfWritingType::Overwrite, NONOTES) as i32;
                                    } else if choice == NO {
                                        return 2; // Discard
                                    } else {
                                        return 0;
                                    }
                                } else if choice == CANCEL && exiting {
                                    continue;
                                } else if choice != YES {
                                    return 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        break;
    }

    // Write the file.  The prompt answer is a (possibly lossy) display string;
    // when it names the buffer's own file, substitute the authoritative path so
    // a non-UTF-8 filename round-trips to the exact original byte sequence.
    let final_answer = state().answer.clone();
    let target: PathBuf = with_state(|s| {
        s.openfile.as_ref().and_then(|of| {
            let path = openfile_filename_path(of)?;
            if of.filename == final_answer {
                Some(path.to_path_buf())
            } else {
                None
            }
        })
    }).unwrap_or_else(|| PathBuf::from(&final_answer));

    #[cfg(not(feature = "tiny"))]
    {
        let mark_on = with_state(|s| {
            s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
        });
        if mark_on && withprompt && !exiting && !ISSET!(RESTRICTED) {
            return write_region_to_file(&target, None, NORMAL, method) as i32;
        }
    }

    write_file(&target, None, NORMAL, method, ANNOTATE) as i32
}

/* C: void do_writeout(void)
 * Write the current buffer to disk, or discard it. */
pub fn do_writeout() {
    if write_it_out(false, true) == 2 {
        close_and_go();
    }
}

/* C: void do_savefile(void)
 * Write the current buffer to disk without prompting (if it has a name). */
pub fn do_savefile() {
    if write_it_out(false, false) == 2 {
        close_and_go();
    }
}

// ---------------------------------------------------------------------------
// expand_leading_tilde — expand ~ in paths
// C: char *expand_leading_tilde(const char *path)
// ---------------------------------------------------------------------------
pub fn expand_leading_tilde(path: &str) -> String {
    if !path.starts_with('~') || path.len() == 1 {
        return path.to_string();
    }

    // Find the end of the username part (~user or ~/)
    let slash_pos = path[1..].find('/').map(|p| p + 1).unwrap_or(path.len());
    let username_part = &path[1..slash_pos];
    let rest = &path[slash_pos..];

    let tilded: String = if username_part.is_empty() {
        // Just ~, use $HOME
        crate::utils::get_homedir();
        state().homedir.clone().unwrap_or_default()
    } else {
        // ~user — look up in passwd
        #[cfg(unix)]
        {
            let cname = match std::ffi::CString::new(username_part) {
                Ok(s) => s,
                Err(_) => return path.to_string(),
            };
            let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
            if pw.is_null() {
                String::new()
            } else {
                unsafe {
                    std::ffi::CStr::from_ptr((*pw).pw_dir)
                        .to_string_lossy()
                        .into_owned()
                }
            }
        }
        #[cfg(not(unix))]
        String::new()
    };

    if tilded.is_empty() {
        path.to_string()
    } else {
        format!("{}{}", tilded, rest)
    }
}

// ---------------------------------------------------------------------------
// diralphasort — sort file listings alphabetically with dirs first
// C: int diralphasort(const void *va, const void *vb)
// ---------------------------------------------------------------------------
#[cfg(any(feature = "tabcomp", feature = "browser"))]
pub fn diralphasort(a: &str, b: &str) -> std::cmp::Ordering {
    let a_is_dir = confined_is_dir(a).unwrap_or(false);
    let b_is_dir = confined_is_dir(b).unwrap_or(false);

    if a_is_dir && !b_is_dir {
        return std::cmp::Ordering::Less;
    }
    if !a_is_dir && b_is_dir {
        return std::cmp::Ordering::Greater;
    }

    // Case-insensitive compare
    let diff = mbstrcasecmp(a, b);
    if diff != 0 {
        if diff < 0 { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater }
    } else {
        a.cmp(b)
    }
}

// ---------------------------------------------------------------------------
// is_dir — return TRUE when the given path is a directory
// C: bool is_dir(const char *path)
// ---------------------------------------------------------------------------
#[cfg(feature = "tabcomp")]
pub fn is_dir(path: &str) -> bool {
    let expanded = expand_leading_tilde(path);
    confined_is_dir(&expanded).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// username_completion — complete a username fragment
// C: char **username_completion(const char *morsel, size_t length, size_t *num_matches)
// ---------------------------------------------------------------------------
#[cfg(feature = "tabcomp")]
pub fn username_completion(morsel: &str, length: usize) -> Vec<String> {
    let mut matches: Vec<String> = Vec::new();

    #[cfg(unix)]
    unsafe {
        libc::setpwent();
        loop {
            let pw = libc::getpwent();
            if pw.is_null() {
                break;
            }
            let name = std::ffi::CStr::from_ptr((*pw).pw_name)
                .to_string_lossy()
                .into_owned();
            // morsel starts with ~, so morsel[1..length-1] is the fragment
            let _fragment = if morsel.len() >= 1 { &morsel[1..length.saturating_sub(0)] } else { "" };
            // Actually compare against morsel+1 .. length-1
            let frag = if morsel.len() > 1 { &morsel[1..] } else { "" };
            if name.starts_with(frag) {
                #[cfg(feature = "operatingdir")]
                {
                    let dir = std::ffi::CStr::from_ptr((*pw).pw_dir)
                        .to_string_lossy()
                        .into_owned();
                    let od = state().operating_dir.clone();
                    if let Some(_od_str) = od {
                        if outside_of_confinement(&dir, true) {
                            continue;
                        }
                    }
                }
                matches.push(format!("~{}", name));
            }
        }
        libc::endpwent();
    }

    matches
}

// ---------------------------------------------------------------------------
// filename_completion — complete a filename fragment
// C: char **filename_completion(const char *morsel, size_t *num_matches)
// ---------------------------------------------------------------------------
#[cfg(feature = "tabcomp")]
pub fn filename_completion(morsel: &str) -> Vec<String> {
    let mut matches: Vec<String> = Vec::new();

    let present_path = with_state(|s| s.present_path.clone().unwrap_or_else(|| "./".to_string()));

    // Split morsel into dirname and filename parts
    let (dirname, filename) = if let Some(slash_pos) = last_path_separator(morsel) {
        let dir_part = &morsel[..=slash_pos];
        let file_part = &morsel[slash_pos + 1..];
        let expanded = expand_leading_tilde(dir_part);
        let full_dir = if Path::new(&expanded).is_absolute() {
            expanded
        } else {
            path_join_display(&present_path, dir_part)
        };
        (full_dir, file_part.to_string())
    } else {
        (present_path.clone(), morsel.to_string())
    };

    let dir = match confined_read_dir(&dirname) {
        Err(_) => {
            beep();
            return matches;
        }
        Ok(d) => d,
    };

    let _filenamelen = filename.len();

    for entry in dir {
        let entry_name = entry.to_string_lossy().into_owned();

        if entry_name == "." || entry_name == ".." {
            continue;
        }

        if entry_name.starts_with(&filename) {
            let fullname = path_join_display(&dirname, &entry_name);

            #[cfg(feature = "operatingdir")]
            {
                if state().operating_dir.is_some() {
                    if outside_of_confinement(&fullname, true) {
                        continue;
                    }
                }
            }

            let currmenu = state().currmenu;
            if currmenu == MGOTODIR && !is_dir(&fullname) {
                continue;
            }

            matches.push(entry_name);
        }
    }

    matches
}

// ---------------------------------------------------------------------------
// input_tab — do tab completion
// C: char *input_tab(char *morsel, size_t *place, void (*refresh_func)(void), bool *listed)
// ---------------------------------------------------------------------------
#[cfg(feature = "tabcomp")]
pub fn input_tab(
    morsel: &str,
    place: &mut usize,
    refresh_func: fn(),
    listed: &mut bool,
) -> String {
    // If the cursor is not at the end of the fragment, do nothing.
    if *place < morsel.len() {
        beep();
        return morsel.to_string();
    }

    let mut matches: Vec<String>;

    // Try username completion if starts with ~ and no slash
    if morsel.starts_with('~') && last_path_separator(morsel).is_none() {
        matches = username_completion(morsel, *place);
    } else {
        matches = Vec::new();
    }

    // If no matches yet, try filename completion
    if matches.is_empty() {
        matches = filename_completion(morsel);
    }

    // If completions were listed before but none will be listed now...
    if *listed && matches.len() < 2 {
        refresh_func();
        *listed = false;
    }

    if matches.is_empty() {
        beep();
        return morsel.to_string();
    }

    // Find last slash in morsel
    let length_of_path = last_path_separator(morsel).map(|p| p + 1).unwrap_or(0);

    // Determine common prefix length
    let mut common_len = 0;
    if !matches.is_empty() {
        let first = &matches[0];
        'outer: loop {
            if common_len >= first.len() {
                break;
            }
            // Get next char boundary
            let ch1 = &first[common_len..];
            let ch1_char = ch1.chars().next();
            let ch1_len = match ch1_char {
                Some(c) => c.len_utf8(),
                None => break,
            };

            for m in &matches[1..] {
                if common_len + ch1_len > m.len() {
                    break 'outer;
                }
                // Compare at the byte level (like C's strncmp); slicing m as a &str
                // could land on a non-char boundary and panic for multibyte names.
                if first.as_bytes()[common_len..common_len + ch1_len]
                    != m.as_bytes()[common_len..common_len + ch1_len] {
                    break 'outer;
                }
            }
            common_len += ch1_len;
        }
    }

    // Build shared prefix: path_prefix + common portion of matches
    let mut shared = format!("{}{}", &morsel[..length_of_path], &matches[0][..common_len]);
    // Append slash if single match that is a directory
    let present_path = with_state(|s| s.present_path.clone().unwrap_or_else(|| "./".to_string()));
    let glued = path_join_display(&present_path, &shared);

    let is_single_dir = matches.len() == 1 && (is_dir(&shared) || is_dir(&glued));
    if is_single_dir {
        shared.push(std::path::MAIN_SEPARATOR);
    }

    // Compare after adding a directory separator.  Comparing the old common
    // prefix length dropped the separator when the user had already typed an
    // exact directory name (for example, `src` + Tab).
    let new_morsel = if shared.len() != *place {
        *place = shared.len();
        shared.clone()
    } else {
        if matches.len() == 1 {
            beep();
        }
        morsel.to_string()
    };

    // Show list if more than one possible completion
    if matches.len() > 1 {
        // Sort matches
        matches.sort_by(|a, b| diralphasort(a, b));

        if !*listed {
            beep();
        }

        crate::winio::show_completion_candidates(&matches);
        *listed = true;
    }

    new_morsel
}

#[cfg(test)]
mod tests {
    use super::{encode_data, read_until_cancelled, temporary_suffix, usable_parent};
    #[cfg(all(not(feature = "tiny"), any(unix, windows)))]
    use super::{create_lockfile, delete_lockfile, open_lockfile_for_create, write_lockfile};
    #[cfg(all(unix, not(feature = "tiny")))]
    use super::open_existing_lockfile;
    use std::io::{self, Cursor, Read};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    #[cfg(feature = "tabcomp")]
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(feature = "tabcomp")]
    static COMPLETION_REFRESHES: AtomicUsize = AtomicUsize::new(0);

    #[cfg(feature = "tabcomp")]
    fn count_completion_refresh() {
        COMPLETION_REFRESHES.fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    static COMMAND_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(feature = "operatingdir")]
    struct TestOperatingRoot {
        previous_path: Option<String>,
        previous_root: Option<super::OperatingRoot>,
    }

    #[cfg(feature = "operatingdir")]
    impl TestOperatingRoot {
        fn install(path: &Path) -> Self {
            let display_path = std::fs::canonicalize(path).unwrap();
            let directory = cap_std::fs::Dir::open_ambient_dir(
                &display_path,
                cap_std::ambient_authority(),
            ).unwrap();
            let previous_root = super::OPERATING_ROOT.with(|slot| {
                slot.borrow_mut().replace(super::OperatingRoot {
                    display_path: display_path.clone(),
                    dir: directory,
                })
            });
            let previous_path = crate::global::state().operating_dir.clone();
            crate::global::state_mut().operating_dir =
                Some(display_path.to_string_lossy().into_owned());
            Self { previous_path, previous_root }
        }
    }

    #[cfg(feature = "operatingdir")]
    impl Drop for TestOperatingRoot {
        fn drop(&mut self) {
            super::OPERATING_ROOT.with(|slot| {
                *slot.borrow_mut() = self.previous_root.take();
            });
            crate::global::state_mut().operating_dir = self.previous_path.take();
        }
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    fn install_test_buffer(lines: &[&str]) -> Vec<crate::definitions::LinePtr> {
        use super::{make_new_buffer, make_new_node};
        use crate::global::{state, with_state_mut};

        assert!(!lines.is_empty());
        make_new_buffer();
        let first = {
            let editor = state();
            editor.openfile.as_ref().unwrap().filetop.clone().unwrap()
        };
        first.borrow_mut().data = lines[0].to_string();
        let mut nodes = vec![first.clone()];
        let mut previous = first;
        for (index, data) in lines.iter().enumerate().skip(1) {
            let node = make_new_node(Some(previous.clone()));
            node.borrow_mut().data = (*data).to_string();
            node.borrow_mut().lineno = (index + 1) as isize;
            previous.borrow_mut().next = Some(node.clone());
            nodes.push(node.clone());
            previous = node;
        }
        with_state_mut(|editor| {
            let buffer = editor.openfile.as_mut().unwrap();
            buffer.filebot = Some(previous);
            buffer.current = Some(nodes[0].clone());
            buffer.current_x = 0;
            buffer.mark = None;
        });
        nodes
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    fn current_buffer_lines() -> Vec<String> {
        use crate::global::state;

        let mut line = {
            let editor = state();
            editor.openfile.as_ref().and_then(|buffer| buffer.filetop.clone())
        };
        let mut lines = Vec::new();
        while let Some(node) = line {
            let borrowed = node.borrow();
            lines.push(borrowed.data.clone());
            line = borrowed.next.clone();
        }
        lines
    }

    #[test]
    fn encode_data_reports_invalid_utf8() {
        let (valid, valid_lossy) = encode_data(b"hello");
        assert_eq!(valid, "hello");
        assert!(!valid_lossy);

        let (invalid, invalid_lossy) = encode_data(&[b'a', 0xff, b'b']);
        assert_eq!(invalid, "a\u{fffd}b");
        assert!(invalid_lossy);
    }

    #[test]
    fn bare_filename_parent_is_current_directory() {
        assert_eq!(usable_parent(Path::new("nano.txt")), Path::new("."));
        assert_eq!(usable_parent(Path::new("./nano.txt")), Path::new("."));
    }

    #[test]
    fn temporary_suffix_uses_only_the_final_path_component() {
        assert_eq!(temporary_suffix("dir.with.dot/file"), "");
        assert_eq!(temporary_suffix("archive.tar.gz"), ".gz");
        assert_eq!(temporary_suffix(".nanorc"), "");
    }

    #[cfg(feature = "tabcomp")]
    #[test]
    fn filename_completion_covers_zero_one_many_hidden_and_unicode_matches() {
        use super::filename_completion;

        let directory = tempfile::tempdir().unwrap();
        for name in ["alpha", "alpine", "solo", ".secret", "猫一", "猫二"] {
            std::fs::write(directory.path().join(name), b"").unwrap();
        }
        crate::global::state_mut().present_path =
            Some(directory.path().to_string_lossy().into_owned());

        assert!(filename_completion("missing").is_empty());
        assert_eq!(filename_completion("sol"), ["solo"]);

        let mut many = filename_completion("al");
        many.sort();
        assert_eq!(many, ["alpha", "alpine"]);

        assert_eq!(filename_completion("."), [".secret"]);

        let mut unicode = filename_completion("猫");
        unicode.sort();
        assert_eq!(unicode, ["猫一", "猫二"]);
    }

    #[cfg(feature = "tabcomp")]
    #[test]
    fn input_tab_appends_exact_directory_with_the_native_separator() {
        use super::input_tab;

        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        std::fs::create_dir_all(nested.join("folder")).unwrap();
        crate::global::state_mut().present_path =
            Some(directory.path().to_string_lossy().into_owned());

        let separator = std::path::MAIN_SEPARATOR;
        let morsel = format!("nested{separator}folder");
        let mut place = morsel.len();
        let mut listed = false;
        let completed = input_tab(&morsel, &mut place, count_completion_refresh, &mut listed);

        assert_eq!(completed, format!("nested{separator}folder{separator}"));
        assert_eq!(place, completed.len());
        assert!(!listed);
    }

    #[cfg(feature = "tabcomp")]
    #[test]
    fn input_tab_lists_many_and_refreshes_a_stale_list_for_zero_or_one() {
        use super::input_tab;

        let directory = tempfile::tempdir().unwrap();
        for name in ["alpha", "alpine", "solo"] {
            std::fs::write(directory.path().join(name), b"").unwrap();
        }
        crate::global::with_state_mut(|state| {
            state.present_path = Some(directory.path().to_string_lossy().into_owned());
            // A zero-height synthetic edit view keeps this behavior test from
            // painting escape sequences; winio's pure tests verify geometry.
            state.editwinrows = 0;
            state.midwin.rows = 0;
            state.midwin.cols = 20;
        });

        COMPLETION_REFRESHES.store(0, Ordering::SeqCst);
        let mut place = 2;
        let mut listed = false;
        let common = input_tab("al", &mut place, count_completion_refresh, &mut listed);
        assert_eq!(common, "alp");
        assert_eq!(place, 3);
        assert!(listed);
        assert_eq!(COMPLETION_REFRESHES.load(Ordering::SeqCst), 0);

        place = "missing".len();
        let unchanged = input_tab("missing", &mut place, count_completion_refresh, &mut listed);
        assert_eq!(unchanged, "missing");
        assert!(!listed);
        assert_eq!(COMPLETION_REFRESHES.load(Ordering::SeqCst), 1);

        // A single completion also removes a previously displayed list.
        listed = true;
        place = 3;
        let single = input_tab("sol", &mut place, count_completion_refresh, &mut listed);
        assert_eq!(single, "solo");
        assert!(!listed);
        assert_eq!(COMPLETION_REFRESHES.load(Ordering::SeqCst), 2);
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn backup_path_encoding_is_separator_free_and_collision_safe() {
        use super::backup_path_key;

        let nested = backup_path_key(Path::new("directory/file"));
        let punctuation = backup_path_key(Path::new("directory!file"));
        assert_ne!(nested, punctuation);
        assert!(!nested.contains(['/','\\']));
        assert!(!punctuation.contains(['/','\\']));
    }

    struct InterruptedOnce {
        interrupted: bool,
        bytes: Cursor<Vec<u8>>,
    }

    impl Read for InterruptedOnce {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            self.bytes.read(buffer)
        }
    }

    #[test]
    fn chunked_reader_retries_eintr_and_stops_on_cancel() {
        let cancelled = AtomicBool::new(false);
        let mut reader = InterruptedOnce {
            interrupted: false,
            bytes: Cursor::new(b"complete".to_vec()),
        };
        let (bytes, was_cancelled) = read_until_cancelled(&mut reader, &cancelled).unwrap();
        assert_eq!(bytes, b"complete");
        assert!(!was_cancelled);

        cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut unread = Cursor::new(b"must not be read".to_vec());
        let (bytes, was_cancelled) = read_until_cancelled(&mut unread, &cancelled).unwrap();
        assert!(bytes.is_empty());
        assert!(was_cancelled);
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn cancelled_read_does_not_ingraft_its_partial_chunk() {
        struct CancelAfterOneChunk(bool);

        impl Read for CancelAfterOneChunk {
            fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Ok(0);
                }
                self.0 = true;
                let partial = b"partial\ncontent";
                destination[..partial.len()].copy_from_slice(partial);
                crate::nano::CONTROL_C_WAS_PRESSED.store(
                    true,
                    std::sync::atomic::Ordering::SeqCst,
                );
                Ok(partial.len())
            }
        }

        install_test_buffer(&["original", ""]);
        assert!(!super::read_file_impl(
            CancelAfterOneChunk(false),
            false,
            "cancelled input",
            true,
        ));
        assert_eq!(current_buffer_lines(), ["original", ""]);
    }

    #[cfg(all(unix, feature = "operatingdir"))]
    #[test]
    fn retained_root_rejects_parent_and_leaf_symlink_swaps_but_follows_inner_links() {
        use super::{open_path, outside_of_confinement};
        use std::io::Read as _;
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = root.path().join("inside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::write(inside.join("document"), b"inside").unwrap();
        std::fs::write(outside.path().join("document"), b"outside").unwrap();
        let _guard = TestOperatingRoot::install(root.path());

        let parent_link = root.path().join("parent");
        symlink("inside", &parent_link).unwrap();
        let through_parent = parent_link.join("document");
        assert!(!outside_of_confinement(
            through_parent.to_str().unwrap(),
            false,
        ));

        // A normal relative symlink whose target remains beneath the root is
        // supported and resolves to the intended object.
        let mut file = open_path(&through_parent).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "inside");

        // Replacing an already-approved parent with an outside symlink cannot
        // redirect the subsequent authority-bearing open.
        std::fs::remove_file(&parent_link).unwrap();
        symlink(outside.path(), &parent_link).unwrap();
        assert!(open_path(&through_parent).is_err());
        assert_eq!(std::fs::read(outside.path().join("document")).unwrap(), b"outside");

        let leaf_link = root.path().join("leaf");
        symlink("inside/document", &leaf_link).unwrap();
        assert!(!outside_of_confinement(leaf_link.to_str().unwrap(), false));
        std::fs::remove_file(&leaf_link).unwrap();
        symlink(outside.path().join("document"), &leaf_link).unwrap();
        assert!(open_path(&leaf_link).is_err());
        assert_eq!(std::fs::read(outside.path().join("document")).unwrap(), b"outside");
    }

    #[cfg(all(unix, feature = "operatingdir", not(feature = "tiny")))]
    #[test]
    fn writes_locks_directory_reads_and_staged_installs_cannot_cross_swapped_parent() {
        use super::{
            confined_read_dir, create_staging_file, open_lockfile_for_create,
            open_path_with, outside_of_confinement, PathOpenOptions,
        };
        use std::io::Write as _;
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = root.path().join("inside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::write(outside.path().join("victim"), b"untouched").unwrap();
        let _guard = TestOperatingRoot::install(root.path());

        let parent_link = root.path().join("parent");
        symlink("inside", &parent_link).unwrap();
        let target = parent_link.join("victim");
        assert!(!outside_of_confinement(target.to_str().unwrap(), false));

        // Create staging while the parent is legitimate, then exchange the
        // parent before both the direct write and the final atomic rename.
        let mut staging = create_staging_file(&parent_link, ".nano-test.").unwrap();
        staging.as_file_mut().write_all(b"candidate").unwrap();
        std::fs::remove_file(&parent_link).unwrap();
        symlink(outside.path(), &parent_link).unwrap();

        assert!(open_path_with(&target, PathOpenOptions {
            write: true,
            create: true,
            truncate: true,
            mode: 0o666,
            ..PathOpenOptions::default()
        }).is_err());
        assert!(open_lockfile_for_create(&parent_link.join(".victim.swp")).is_err());
        assert!(confined_read_dir(parent_link.to_str().unwrap()).is_err());
        assert!(staging.persist(&target).is_err());

        assert_eq!(std::fs::read(outside.path().join("victim")).unwrap(), b"untouched");
        assert!(!outside.path().join(".victim.swp").exists());
    }

    #[cfg(all(windows, feature = "operatingdir"))]
    #[test]
    fn capability_staging_replaces_an_existing_windows_file() {
        use super::create_staging_file;
        use std::io::Write as _;

        let root = tempfile::tempdir().unwrap();
        let root_path = std::fs::canonicalize(root.path()).unwrap();
        let nested = root_path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let target = nested.join("document");
        std::fs::write(&target, b"old contents").unwrap();
        let _guard = TestOperatingRoot::install(&root_path);

        let mut staging = create_staging_file(&nested, ".nano-test.").unwrap();
        staging.as_file_mut().write_all(b"new contents").unwrap();
        staging.as_file().sync_all().unwrap();
        staging.persist(&target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new contents");
    }

    #[cfg(all(unix, feature = "operatingdir"))]
    #[test]
    fn replacing_the_operating_directory_name_does_not_replace_its_capability() {
        use super::open_path;
        use std::io::Read as _;

        let holder = tempfile::tempdir().unwrap();
        let named_root = holder.path().join("root");
        let displaced_root = holder.path().join("displaced");
        std::fs::create_dir(&named_root).unwrap();
        std::fs::write(named_root.join("document"), b"original root").unwrap();
        let _guard = TestOperatingRoot::install(&named_root);

        std::fs::rename(&named_root, &displaced_root).unwrap();
        std::fs::create_dir(&named_root).unwrap();
        std::fs::write(named_root.join("document"), b"replacement root").unwrap();

        let mut file = open_path(&named_root.join("document")).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "original root");
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn lock_creation_is_exclusive_and_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("existing.swp");
        std::fs::write(&existing, b"owned by someone else").unwrap();
        assert!(open_lockfile_for_create(&existing).is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"owned by someone else");

        let victim = directory.path().join("victim");
        let link = directory.path().join("link.swp");
        std::fs::write(&victim, b"do not truncate").unwrap();
        symlink(&victim, &link).unwrap();
        assert!(open_lockfile_for_create(&link).is_err());
        assert!(open_existing_lockfile(&link).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"do not truncate");
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn concurrent_lock_acquisition_has_one_winner() {
        use std::sync::{Arc, Barrier};

        let directory = tempfile::tempdir().unwrap();
        let path = Arc::new(directory.path().join("contended.swp"));
        let barrier = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    open_lockfile_for_create(&path).is_ok()
                })
            })
            .collect();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
    }

    #[cfg(all(not(feature = "tiny"), any(unix, windows)))]
    #[test]
    fn retained_lock_descriptor_rejects_writes_and_unlinks_after_path_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owned.swp");
        let displaced = directory.path().join("displaced.swp");
        let mut held = create_lockfile(&path, Path::new("document"), false).unwrap();
        std::fs::rename(&path, &displaced).unwrap();
        std::fs::write(&path, b"racing editor").unwrap();

        assert!(!write_lockfile(
            &mut held,
            &path,
            Path::new("document"),
            true,
        ));
        assert!(!delete_lockfile(path.to_str().unwrap(), Some(&held)));
        assert_eq!(std::fs::read(&path).unwrap(), b"racing editor");
    }

    #[cfg(all(not(feature = "tiny"), any(unix, windows)))]
    #[test]
    fn retained_lock_descriptor_allows_its_own_name_to_be_unlinked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owned.swp");
        let held = create_lockfile(&path, Path::new("document"), false).unwrap();

        assert!(delete_lockfile(path.to_str().unwrap(), Some(&held)));
        assert!(!path.exists());
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn backup_replacement_is_complete_and_does_not_follow_destination_symlink() {
        use super::{make_backup_of, stat_with_alloc};
        use crate::global::state_mut;
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("document");
        let backup = directory.path().join("document~");
        let victim = directory.path().join("unrelated");
        std::fs::write(&original, b"complete new backup").unwrap();
        std::fs::write(&victim, b"must survive").unwrap();
        symlink(&victim, &backup).unwrap();
        state_mut().backup_dir = None;

        let info = stat_with_alloc(original.to_str().unwrap()).unwrap();
        assert!(make_backup_of(&original, &info));
        assert_eq!(std::fs::read(&backup).unwrap(), b"complete new backup");
        assert_eq!(std::fs::read(&victim).unwrap(), b"must survive");
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn non_utf8_filename_round_trips_through_open_edit_save_and_backup() {
        use crate::global::{state, state_mut, with_state};
        use std::os::unix::ffi::OsStrExt;

        // open_buffer_impl swaps the SIGINT handler around the read; keep the
        // command tests (which raise SIGINT) from overlapping with that window.
        let _serial = COMMAND_TEST_LOCK.lock().unwrap();

        let directory = tempfile::tempdir().unwrap();
        let weird = std::ffi::OsStr::from_bytes(b"weird-\xFF-name");
        let target = directory.path().join(weird);
        std::fs::write(&target, b"first line\n").unwrap();

        state_mut().flags = [0; 4];
        state_mut().backup_dir = None;
        assert!(super::open_buffer_impl(&target, true));

        // The authoritative path keeps the exact bytes; the display string is
        // the lossy rendering used only for the UI.
        let (stored_path, stored_display) = with_state(|editor| {
            let buffer = editor.openfile.as_ref().unwrap();
            (buffer.filename_path.clone(), buffer.filename.clone())
        });
        assert_eq!(
            stored_path.file_name().unwrap().as_bytes(),
            b"weird-\xFF-name"
        );
        assert!(stored_display.contains('\u{fffd}'));
        assert_eq!(current_buffer_lines(), ["first line", ""]);

        // Edit the buffer, then save through the unprompted write_it_out path
        // (the do_savefile flow), with backups enabled.
        let first = state().openfile.as_ref().unwrap().filetop.clone().unwrap();
        first.borrow_mut().data = "second version".to_string();
        crate::SET!(crate::definitions::MAKE_BACKUP);
        assert_eq!(super::write_it_out(false, false), 1);

        // The save landed at the exact original byte-name, and the backup at
        // its byte-preserved sibling "<name>~" ...
        assert_eq!(std::fs::read(&target).unwrap(), b"second version\n");
        let mut backup_name = weird.to_os_string();
        backup_name.push("~");
        let backup = directory.path().join(&backup_name);
        assert_eq!(std::fs::read(&backup).unwrap(), b"first line\n");

        // ... and NOT at any lossily-converted name.
        assert!(!directory.path().join("weird-\u{fffd}-name").exists());
        assert!(!directory.path().join("weird-\u{fffd}-name~").exists());
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn lockfile_for_non_utf8_filename_uses_byte_preserved_sibling_name() {
        use std::os::unix::ffi::OsStrExt;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join(std::ffi::OsStr::from_bytes(b"weird-\xFF-name"));
        std::fs::write(&target, b"content\n").unwrap();

        let (lock_path, lock_file) = super::do_lockfile(&target, false)
            .unwrap()
            .expect("lock creation should succeed");

        // The lock is the byte-preserved sibling ".<name>.swp" ...
        assert_eq!(
            lock_path.file_name().unwrap().as_bytes(),
            b".weird-\xFF-name.swp"
        );
        let lockdata = std::fs::read(&lock_path).unwrap();
        assert_eq!(lockdata.len(), super::LOCKSIZE);
        // ... and it records the locked file's exact bytes, not a lossy form.
        let recorded = &lockdata[108..108 + target.as_os_str().as_bytes().len()];
        assert_eq!(recorded, target.as_os_str().as_bytes());
        assert!(!directory.path().join(".weird-\u{fffd}-name.swp").exists());

        // Deletion also goes through the byte-preserved name.
        assert!(super::delete_lockfile(&lock_path, Some(&lock_file)));
        assert!(!lock_path.exists());
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn prepend_installs_only_the_complete_staged_result() {
        use super::{make_new_buffer, make_new_node, write_file};
        use crate::definitions::KindOfWritingType;
        use crate::global::{state, state_mut, with_state_mut};

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("document");
        std::fs::write(&target, b"old contents\n").unwrap();

        state_mut().flags = [0; 4];
        make_new_buffer();
        let first = state().openfile.as_ref().unwrap().filetop.clone().unwrap();
        first.borrow_mut().data = "new contents".to_string();
        let end = make_new_node(Some(first.clone()));
        end.borrow_mut().lineno = 2;
        first.borrow_mut().next = Some(end.clone());
        with_state_mut(|editor| {
            let buffer = editor.openfile.as_mut().unwrap();
            buffer.filebot = Some(end);
        });

        assert!(write_file(
            target.to_str().unwrap(),
            None,
            true,
            KindOfWritingType::Prepend,
            false,
        ));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"new contents\nold contents\n"
        );
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .all(|entry| !entry.unwrap().file_name().to_string_lossy().starts_with(".nano-prepend.")));
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn command_session_restores_signal_termios_tracking_state() {
        use super::{
            cancel_command_trampoline, CommandSession, PID_OF_COMMAND, PID_OF_SENDER,
            SHOULD_PIPE,
        };
        use std::sync::atomic::Ordering;

        let _serial = COMMAND_TEST_LOCK.lock().unwrap();
        let mut before: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &mut before) },
            0
        );

        let session = CommandSession::start(false).unwrap();
        let mut during: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &mut during) },
            0
        );
        assert_eq!(
            during.sa_sigaction,
            cancel_command_trampoline as *const () as libc::sighandler_t
        );
        PID_OF_COMMAND.store(41, Ordering::SeqCst);
        PID_OF_SENDER.store(42, Ordering::SeqCst);
        SHOULD_PIPE.store(true, Ordering::SeqCst);
        drop(session);

        let mut after: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &mut after) },
            0
        );
        assert_eq!(after.sa_sigaction, before.sa_sigaction);
        assert_eq!(PID_OF_COMMAND.load(Ordering::SeqCst), -1);
        assert_eq!(PID_OF_SENDER.load(Ordering::SeqCst), -1);
        assert!(!SHOULD_PIPE.load(Ordering::SeqCst));
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn command_snapshot_uses_marked_region_or_full_buffer() {
        use super::command_input_snapshot;
        use crate::global::with_state_mut;

        let nodes = install_test_buffer(&["alpha", "beta", ""]);
        assert_eq!(command_input_snapshot(), b"alpha\nbeta\n");

        with_state_mut(|editor| {
            let buffer = editor.openfile.as_mut().unwrap();
            buffer.mark = Some(nodes[0].clone());
            buffer.mark_x = 2;
            buffer.current = Some(nodes[1].clone());
            buffer.current_x = 2;
        });
        assert_eq!(command_input_snapshot(), b"pha\nbe");
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn successful_filter_replaces_once_and_undo_restores_source() {
        use super::execute_command;

        let _serial = COMMAND_TEST_LOCK.lock().unwrap();
        install_test_buffer(&["hello", ""]);
        execute_command("|tr a-z A-Z");
        assert_eq!(current_buffer_lines(), ["HELLO", ""]);

        crate::text::do_undo();
        assert_eq!(current_buffer_lines(), ["hello", ""]);
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn failed_filter_leaves_source_unchanged() {
        use super::execute_command;

        let _serial = COMMAND_TEST_LOCK.lock().unwrap();
        install_test_buffer(&["original", ""]);
        execute_command("|sh -c 'printf changed; printf failure >&2; exit 7'");
        assert_eq!(current_buffer_lines(), ["original", ""]);
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn ctrl_c_cancels_command_and_restores_tracking() {
        use super::{execute_command, PID_OF_COMMAND, PID_OF_SENDER};
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        let _serial = COMMAND_TEST_LOCK.lock().unwrap();
        install_test_buffer(&["unchanged", ""]);
        let interrupter = std::thread::spawn(|| {
            for _ in 0..200 {
                if PID_OF_COMMAND.load(Ordering::SeqCst) > 0 {
                    unsafe { libc::kill(libc::getpid(), libc::SIGINT) };
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("command PID was never published");
        });

        let started = Instant::now();
        execute_command("|sleep 10");
        interrupter.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(current_buffer_lines(), ["unchanged", ""]);
        assert_eq!(PID_OF_COMMAND.load(Ordering::SeqCst), -1);
        assert_eq!(PID_OF_SENDER.load(Ordering::SeqCst), -1);
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    #[test]
    fn filter_replaces_only_the_marked_region() {
        use super::execute_command;
        use crate::global::with_state_mut;

        let _serial = COMMAND_TEST_LOCK.lock().unwrap();
        let nodes = install_test_buffer(&["hello world", ""]);
        with_state_mut(|editor| {
            let buffer = editor.openfile.as_mut().unwrap();
            buffer.mark = Some(nodes[0].clone());
            buffer.mark_x = 6;
            buffer.current = Some(nodes[0].clone());
            buffer.current_x = 11;
        });
        execute_command("|tr a-z A-Z");
        assert_eq!(current_buffer_lines(), ["hello WORLD", ""]);
    }
}
