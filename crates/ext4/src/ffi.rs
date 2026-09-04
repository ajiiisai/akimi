use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub(crate) struct FilesystemInfo {
    pub block_size: u32,
    pub first_inode: u32,
    pub inodes_per_group: u32,
    pub group_count: u32,
    pub inode_count: u64,
    pub allocated_inode_count: u64,
    pub feature_compat: u32,
    pub feature_incompat: u32,
    pub feature_ro_compat: u32,
    pub is_ext4: u8,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct Inode {
    pub inode: u64,
    pub logical_size: u64,
    pub allocated_size: u64,
    pub links: u32,
    pub mtime: i64,
    pub kind: u8,
}

/// One callback per ~1k inodes instead of one per inode. The slice is only
/// valid for the duration of the call; the caller copies what it needs.
type InodeBatchCallback = unsafe extern "C" fn(*const Inode, usize, *mut c_void) -> c_int;
/// One callback per directory. All slices are only valid for the call.
/// `names` holds every entry name packed back to back with total length
/// `names_len`; `offsets[i]`/`lengths[i]` locate entry `i` inside it.
type DirectoryBatchCallback = unsafe extern "C" fn(
    u64,
    *const u64,
    *const u32,
    *const u16,
    *const u8,
    usize,
    usize,
    *mut c_void,
) -> c_int;

unsafe extern "C" {
    fn akimi_ext4_open(
        path: *const c_char,
        info: *mut FilesystemInfo,
        result: *mut *mut c_void,
    ) -> i64;
    fn akimi_ext4_open_fd(fd: c_int, info: *mut FilesystemInfo, result: *mut *mut c_void) -> i64;
    fn akimi_ext4_close(handle: *mut c_void);
    fn akimi_ext4_load_inode_bitmap(handle: *mut c_void) -> i64;
    fn akimi_ext4_scan_inodes_batched(
        handle: *mut c_void,
        first_inode: u64,
        last_inode: u64,
        callback: InodeBatchCallback,
        private_data: *mut c_void,
    ) -> i64;
    fn akimi_ext4_scan_directory_batched(
        handle: *mut c_void,
        directory: u64,
        callback: DirectoryBatchCallback,
        private_data: *mut c_void,
    ) -> i64;
    fn akimi_ext4_error_message(error: i64) -> *const c_char;
}

#[derive(Debug)]
pub(crate) enum NativeError {
    PathContainsNul,
    Library { operation: &'static str, code: i64 },
    CallbackPanicked,
}

impl NativeError {
    pub fn code(&self) -> Option<i64> {
        match self {
            Self::Library { code, .. } => Some(*code),
            _ => None,
        }
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathContainsNul => formatter.write_str("device path contains a NUL byte"),
            Self::CallbackPanicked => formatter.write_str("filesystem callback panicked"),
            Self::Library { operation, code } => {
                let message = error_text(*code);
                write!(formatter, "{operation}: {message} (libext2fs error {code})")
            }
        }
    }
}

impl std::error::Error for NativeError {}

pub(crate) struct Handle {
    raw: NonNull<c_void>,
    info: FilesystemInfo,
}

// A handle is never shared between threads. Moving sole ownership to a worker
// is safe because that worker performs all libext2fs calls and drops the handle.
unsafe impl Send for Handle {}

impl Handle {
    pub fn open(path: &Path) -> Result<Self, NativeError> {
        let path =
            CString::new(path.as_os_str().as_bytes()).map_err(|_| NativeError::PathContainsNul)?;
        let mut info = FilesystemInfo::default();
        let mut raw = std::ptr::null_mut();
        // The C shim calls ext2fs_open without EXT2_FLAG_RW. A successful call
        // owns one libext2fs handle, which Drop closes.
        let code = unsafe { akimi_ext4_open(path.as_ptr(), &mut info, &mut raw) };
        check("opening filesystem", code)?;
        let raw = NonNull::new(raw).ok_or(NativeError::Library {
            operation: "opening filesystem",
            code: -1,
        })?;
        Ok(Self { raw, info })
    }

    pub fn open_fd(fd: RawFd) -> Result<Self, NativeError> {
        let mut info = FilesystemInfo::default();
        let mut raw = std::ptr::null_mut();
        // unixfd_io_manager borrows this descriptor. Ext4Filesystem keeps the
        // owning File alive until every native handle has closed.
        let code = unsafe { akimi_ext4_open_fd(fd, &mut info, &mut raw) };
        check("opening filesystem descriptor", code)?;
        let raw = NonNull::new(raw).ok_or(NativeError::Library {
            operation: "opening filesystem descriptor",
            code: -1,
        })?;
        Ok(Self { raw, info })
    }

    pub fn info(&self) -> FilesystemInfo {
        self.info
    }

    /// Reads the inode allocation bitmaps once. Later scans assume the
    /// bitmaps are loaded (they fall back to loading them if not).
    pub fn load_inode_bitmap(&mut self) -> Result<(), NativeError> {
        let code = unsafe { akimi_ext4_load_inode_bitmap(self.raw.as_ptr()) };
        check("reading inode bitmap", code)
    }

    pub fn scan_inodes_batched<F>(
        &mut self,
        first_inode: u64,
        last_inode: u64,
        mut callback: F,
    ) -> Result<(), NativeError>
    where
        F: FnMut(&[Inode]) -> bool,
    {
        struct Context<'a, F> {
            callback: &'a mut F,
            panicked: bool,
        }

        unsafe extern "C" fn trampoline<F>(
            items: *const Inode,
            count: usize,
            data: *mut c_void,
        ) -> c_int
        where
            F: FnMut(&[Inode]) -> bool,
        {
            // libext2fs invokes this callback synchronously. The buffer is
            // reused for the next batch, so the callback must copy out of it.
            let context = unsafe { &mut *data.cast::<Context<'_, F>>() };
            let result = catch_unwind(AssertUnwindSafe(|| {
                let batch = unsafe { std::slice::from_raw_parts(items, count) };
                (context.callback)(batch)
            }));
            match result {
                Ok(true) => 0,
                Ok(false) => 1,
                Err(_) => {
                    context.panicked = true;
                    1
                }
            }
        }

        let mut context = Context {
            callback: &mut callback,
            panicked: false,
        };
        let code = unsafe {
            akimi_ext4_scan_inodes_batched(
                self.raw.as_ptr(),
                first_inode,
                last_inode,
                trampoline::<F>,
                (&mut context as *mut Context<'_, F>).cast(),
            )
        };
        if context.panicked {
            return Err(NativeError::CallbackPanicked);
        }
        check("scanning inodes", code)
    }

    pub fn scan_directory_batched<F>(
        &mut self,
        directory: u64,
        mut callback: F,
    ) -> Result<(), NativeError>
    where
        F: FnMut(u64, &[u64], &[u32], &[u16], &[u8]) -> bool,
    {
        struct Context<'a, F> {
            callback: &'a mut F,
            panicked: bool,
        }

        unsafe extern "C" fn trampoline<F>(
            parent: u64,
            children: *const u64,
            offsets: *const u32,
            lengths: *const u16,
            names: *const u8,
            names_len: usize,
            count: usize,
            data: *mut c_void,
        ) -> c_int
        where
            F: FnMut(u64, &[u64], &[u32], &[u16], &[u8]) -> bool,
        {
            // libext2fs owns every buffer. Nothing here may escape this
            // callback; the caller copies names into the name arena.
            let context = unsafe { &mut *data.cast::<Context<'_, F>>() };
            let result = catch_unwind(AssertUnwindSafe(|| {
                let children = unsafe { std::slice::from_raw_parts(children, count) };
                let offsets = unsafe { std::slice::from_raw_parts(offsets, count) };
                let lengths = unsafe { std::slice::from_raw_parts(lengths, count) };
                let names = unsafe { std::slice::from_raw_parts(names, names_len) };
                (context.callback)(parent, children, offsets, lengths, names)
            }));
            match result {
                Ok(true) => 0,
                Ok(false) => 1,
                Err(_) => {
                    context.panicked = true;
                    1
                }
            }
        }

        let mut context = Context {
            callback: &mut callback,
            panicked: false,
        };
        let code = unsafe {
            akimi_ext4_scan_directory_batched(
                self.raw.as_ptr(),
                directory,
                trampoline::<F>,
                (&mut context as *mut Context<'_, F>).cast(),
            )
        };
        if context.panicked {
            return Err(NativeError::CallbackPanicked);
        }
        check("scanning directory", code)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Handle::open is the only constructor, and ownership is not shared.
        unsafe { akimi_ext4_close(self.raw.as_ptr()) };
    }
}

fn check(operation: &'static str, code: i64) -> Result<(), NativeError> {
    if code == 0 {
        Ok(())
    } else {
        Err(NativeError::Library { operation, code })
    }
}

fn error_text(code: i64) -> String {
    // com_err returns a process-lifetime string for known error tables. It can
    // also return a thread-local fallback string for unknown numeric codes.
    let message = unsafe { akimi_ext4_error_message(code) };
    if message.is_null() {
        return "unknown error".to_owned();
    }
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}
