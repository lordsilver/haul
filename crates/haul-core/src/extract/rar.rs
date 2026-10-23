//! RAR volumes, through RARLAB's unrar library.

use std::{fs::File, io, path::Path, sync::atomic::Ordering};

use super::{Part, Progress, enclosed};
use crate::{parts::PartName, paths};

/// RAR volumes find each other by name in one folder. Parts that sit
/// together under their own names are read where they are; otherwise each is
/// linked into the hidden folder under its name (copied when the disk takes
/// no links). Pieces cut by a file splitter (`name.rar.001`) are joined
/// there into the one file they were.
pub(super) fn rar(
    parts: &[Part],
    staging: &Path,
    out: &Path,
    progress: &Progress,
) -> Result<(), String> {
    let first_dir = parts[0].path.parent();
    let in_place = parts.iter().all(|p| {
        p.path.parent() == first_dir
            && p.path.file_name().and_then(|n| n.to_str()) == Some(paths::safe(&p.name).as_str())
    });
    let pieces = PartName::parse(&parts[0].name).filter(|p| p.form.is_pieces());
    let first = if let Some(pieces) = pieces {
        let vol = staging.join("vol");
        std::fs::create_dir_all(&vol).map_err(|e| e.to_string())?;
        let joined = vol.join(paths::safe(&pieces.archive));
        let mut to = File::create(&joined).map_err(|e| format!("{}: {e}", joined.display()))?;
        for p in parts {
            let mut from = File::open(&p.path).map_err(|e| format!("{}: {e}", p.path.display()))?;
            io::copy(&mut from, &mut to).map_err(|e| format!("joining the pieces: {e}"))?;
        }
        joined
    } else if in_place {
        parts[0].path.clone()
    } else {
        let vol = staging.join("vol");
        std::fs::create_dir_all(&vol).map_err(|e| e.to_string())?;
        for p in parts {
            let link = vol.join(paths::safe(&p.name));
            if std::fs::hard_link(&p.path, &link).is_err() {
                std::fs::copy(&p.path, &link).map_err(|e| format!("{}: {e}", p.path.display()))?;
            }
        }
        vol.join(paths::safe(&parts[0].name))
    };
    let mut total = 0;
    let mut listing = dll::Rar::open(&first, false, &progress.done)?;
    while let Some(entry) = listing.next()? {
        if entry.encrypted {
            return Err(dll::NEEDS_PASSWORD.into());
        }
        if !entry.dir {
            total += entry.size;
        }
        listing.skip()?;
    }
    progress.total.store(total, Ordering::Relaxed);

    let mut archive = dll::Rar::open(&first, true, &progress.done)?;
    while let Some(entry) = archive.next()? {
        // A name that would lead out of the folder is left out.
        let Some(rel) = enclosed(&entry.name) else {
            tracing::warn!("left out of the RAR: {}", entry.name);
            archive.skip()?;
            continue;
        };
        archive.extract_to(&out.join(rel))?;
    }
    Ok(())
}

/// RARLAB's own unrar library, through the C interface `unrar_sys` links.
/// The `unrar` crate around it reads 2048 characters from the shorter name
/// of every next volume, past its end, and `unrar_sys` lays its structures
/// out without the byte packing `dll.hpp` gives them, so this stays a few
/// calls of our own with structures of our own.
mod dll {
    use std::{
        ffi::{c_char, c_int, c_uint},
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
    };

    use unrar_sys as sys;

    pub const NEEDS_PASSWORD: &str = "the archive needs a password";

    /// `RAROpenArchiveDataEx` as the bundled `dll.hpp` has it, packed.
    #[repr(C, packed)]
    struct Open {
        arc_name: *const c_char,
        arc_name_w: *const sys::WCHAR,
        open_mode: c_uint,
        open_result: c_uint,
        cmt_buf: *mut c_char,
        cmt_buf_size: c_uint,
        cmt_size: c_uint,
        cmt_state: c_uint,
        flags: c_uint,
        callback: Option<Callback>,
        user_data: sys::LPARAM,
        op_flags: c_uint,
        cmt_buf_w: *mut sys::WCHAR,
        reserved: [c_uint; 25],
    }

    type Callback = extern "system" fn(sys::UINT, sys::LPARAM, sys::LPARAM, sys::LPARAM) -> c_int;

    /// `RARHeaderDataEx` as the bundled `dll.hpp` has it, packed, and with
    /// the fields it has gained since `unrar_sys` copied it.
    #[repr(C, packed)]
    struct Header {
        arc_name: [c_char; 1024],
        arc_name_w: [sys::WCHAR; 1024],
        file_name: [c_char; 1024],
        file_name_w: [sys::WCHAR; 1024],
        flags: c_uint,
        pack_size: c_uint,
        pack_size_high: c_uint,
        unp_size: c_uint,
        unp_size_high: c_uint,
        host_os: c_uint,
        file_crc: c_uint,
        file_time: c_uint,
        unp_ver: c_uint,
        method: c_uint,
        file_attr: c_uint,
        cmt_buf: *mut c_char,
        cmt_buf_size: c_uint,
        cmt_size: c_uint,
        cmt_state: c_uint,
        dict_size: c_uint,
        hash_type: c_uint,
        hash: [c_char; 32],
        redir_type: c_uint,
        redir_name: *mut sys::WCHAR,
        redir_name_size: c_uint,
        dir_target: c_uint,
        mtime_low: c_uint,
        mtime_high: c_uint,
        ctime_low: c_uint,
        ctime_high: c_uint,
        atime_low: c_uint,
        atime_high: c_uint,
        arc_name_ex: *mut sys::WCHAR,
        arc_name_ex_size: c_uint,
        file_name_ex: *mut sys::WCHAR,
        file_name_ex_size: c_uint,
        reserved: [c_uint; 982],
    }

    /// The longest entry name read, in characters.
    const NAME_MAX: usize = 4096;
    /// Asks unrar whether a dictionary bigger than its default limit may
    /// be used; the archive was made with it, so it may.
    const UCM_LARGEDICT: c_uint = 5;

    pub struct Entry {
        pub name: String,
        pub size: u64,
        pub dir: bool,
        pub encrypted: bool,
    }

    /// An open archive: its first volume, read through every next one.
    pub struct Rar<'a> {
        handle: *const sys::Handle,
        header: Box<Header>,
        name: Vec<sys::WCHAR>,
        _done: &'a AtomicU64,
    }

    /// unrar asks along the way: for the next volume (it is either there or
    /// missing, nobody can bring it), for a password (there is none), and
    /// for a big dictionary; and it hands over each piece it unpacks, which
    /// counts toward `done`.
    extern "system" fn callback(
        msg: sys::UINT,
        user: sys::LPARAM,
        _p1: sys::LPARAM,
        p2: sys::LPARAM,
    ) -> c_int {
        match msg {
            sys::UCM_CHANGEVOLUME | sys::UCM_CHANGEVOLUMEW => {
                if p2 == sys::RAR_VOL_ASK {
                    -1
                } else {
                    0
                }
            }
            sys::UCM_PROCESSDATA => {
                // SAFETY: `user` is the `&AtomicU64` that `Rar::open` set,
                // which outlives the handle.
                let done = unsafe { &*(user as *const AtomicU64) };
                done.fetch_add(p2 as u64, Ordering::Relaxed);
                0
            }
            sys::UCM_NEEDPASSWORD | sys::UCM_NEEDPASSWORDW => -1,
            UCM_LARGEDICT => 1,
            _ => 0,
        }
    }

    fn error(code: c_int) -> String {
        match code {
            sys::ERAR_MISSING_PASSWORD | sys::ERAR_BAD_PASSWORD => NEEDS_PASSWORD.into(),
            sys::ERAR_EOPEN => "a part is missing".into(),
            sys::ERAR_BAD_DATA => "the archive is damaged (CRC error)".into(),
            sys::ERAR_BAD_ARCHIVE | sys::ERAR_UNKNOWN_FORMAT => "not a RAR archive".into(),
            sys::ERAR_NO_MEMORY => "not enough memory".into(),
            sys::ERAR_ECREATE => "could not create a file".into(),
            sys::ERAR_EWRITE => "could not write (is the disk full?)".into(),
            sys::ERAR_EREAD => "could not read the archive".into(),
            code => format!("unrar error {code}"),
        }
    }

    /// A path as unrar takes it: on Linux the bytes as they are, which it
    /// carries through its conversions unchanged whatever the locale; on
    /// Windows wide characters.
    #[cfg(any(target_os = "linux", target_os = "netbsd"))]
    fn native(path: &Path) -> Result<Vec<u8>, String> {
        let mut bytes = path.as_os_str().as_encoded_bytes().to_vec();
        if bytes.contains(&0) {
            return Err(format!("{}: a nul in the path", path.display()));
        }
        bytes.push(0);
        Ok(bytes)
    }

    #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
    fn native(path: &Path) -> Result<Vec<sys::WCHAR>, String> {
        #[cfg(windows)]
        let mut wide: Vec<sys::WCHAR> =
            std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str()).collect();
        #[cfg(not(windows))]
        let mut wide: Vec<sys::WCHAR> = path
            .to_string_lossy()
            .chars()
            .map(|c| c as sys::WCHAR)
            .collect();
        if wide.contains(&0) {
            return Err(format!("{}: a nul in the path", path.display()));
        }
        wide.push(0);
        Ok(wide)
    }

    impl<'a> Rar<'a> {
        /// Opens `first` to list it, or to unpack it (`extract`).
        pub fn open(first: &Path, extract: bool, done: &'a AtomicU64) -> Result<Self, String> {
            let name = native(first)?;
            let mode = if extract {
                sys::RAR_OM_EXTRACT
            } else {
                sys::RAR_OM_LIST
            };
            // SAFETY: as for `Header` below.
            let mut data: Open = unsafe { std::mem::zeroed() };
            #[cfg(any(target_os = "linux", target_os = "netbsd"))]
            {
                data.arc_name = name.as_ptr().cast();
            }
            #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
            {
                data.arc_name_w = name.as_ptr().cast();
            }
            data.open_mode = mode;
            data.callback = Some(callback);
            data.user_data = done as *const AtomicU64 as sys::LPARAM;
            // SAFETY: `data` and the name it points to live through the call,
            // and `Open` is laid out as unrar reads it.
            let handle = unsafe { sys::RAROpenArchiveEx((&raw const data).cast()) };
            if handle.is_null() {
                return Err(error(data.open_result as c_int));
            }
            // SAFETY: every field of `Header` is an integer, an array of them,
            // a raw pointer or an optional function, for all of which zero is
            // a valid value.
            let header: Box<Header> = Box::new(unsafe { std::mem::zeroed() });
            Ok(Self {
                handle,
                header,
                name: vec![0; NAME_MAX],
                _done: done,
            })
        }

        /// The next entry, which has to be skipped or extracted before
        /// the one after it.
        pub fn next(&mut self) -> Result<Option<Entry>, String> {
            self.name.fill(0);
            self.header.file_name_ex = self.name.as_mut_ptr();
            self.header.file_name_ex_size = NAME_MAX as c_uint;
            // SAFETY: the handle is open, and the header and the name buffer
            // it points to are as big as unrar is told.
            let code = unsafe { sys::RARReadHeaderEx(self.handle, (&raw mut *self.header).cast()) };
            match code {
                sys::ERAR_SUCCESS => {}
                sys::ERAR_END_ARCHIVE => return Ok(None),
                code => return Err(error(code)),
            }
            let end = self.name.iter().position(|&c| c == 0).unwrap_or(NAME_MAX);
            #[allow(clippy::unnecessary_cast)]
            let name = self.name[..end]
                .iter()
                .map(|&c| c as u32)
                .collect::<Vec<u32>>();
            let name = if cfg!(windows) {
                let units: Vec<u16> = name.iter().map(|&c| c as u16).collect();
                String::from_utf16_lossy(&units)
            } else {
                name.iter()
                    .map(|&c| char::from_u32(c).unwrap_or('\u{fffd}'))
                    .collect()
            };
            let h = &self.header;
            Ok(Some(Entry {
                name,
                size: u64::from(h.unp_size_high) << 32 | u64::from(h.unp_size),
                dir: h.flags & sys::RHDF_DIRECTORY != 0,
                encrypted: h.flags & sys::RHDF_ENCRYPTED != 0,
            }))
        }

        pub fn skip(&mut self) -> Result<(), String> {
            self.process(sys::RAR_SKIP, None)
        }

        /// Unpacks the current entry as `path`, its folders made first.
        pub fn extract_to(&mut self, path: &Path) -> Result<(), String> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            self.process(sys::RAR_EXTRACT, Some(path))
        }

        fn process(&mut self, op: c_int, dest: Option<&Path>) -> Result<(), String> {
            let dest = dest.map(native).transpose()?;
            let dest_ptr = dest.as_ref().map_or(std::ptr::null(), |d| d.as_ptr());
            // SAFETY: the handle is open and `dest` outlives the call.
            #[cfg(any(target_os = "linux", target_os = "netbsd"))]
            let code =
                unsafe { sys::RARProcessFile(self.handle, op, std::ptr::null(), dest_ptr.cast()) };
            #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
            let code =
                unsafe { sys::RARProcessFileW(self.handle, op, std::ptr::null(), dest_ptr.cast()) };
            match code {
                sys::ERAR_SUCCESS => Ok(()),
                code => Err(error(code)),
            }
        }
    }

    impl Drop for Rar<'_> {
        fn drop(&mut self) {
            // SAFETY: opened in `open` and closed only here.
            unsafe { sys::RARCloseArchive(self.handle) };
        }
    }

    /// The offsets `offsetof` gives for `dll.hpp` on 64-bit Linux (4-byte
    /// `wchar_t`) and 64-bit Windows (2-byte).
    #[cfg(all(test, target_pointer_width = "64", any(target_os = "linux", windows)))]
    #[test]
    fn structures_match_dll_hpp() {
        use std::mem::{offset_of, size_of};
        let (size, arc_name_ex) = if cfg!(windows) {
            (10244, 6292)
        } else {
            (14340, 10388)
        };
        assert_eq!(size_of::<Header>(), size);
        assert_eq!(offset_of!(Header, arc_name_ex), arc_name_ex);
        assert_eq!(offset_of!(Header, file_name_ex), arc_name_ex + 12);
        assert_eq!(offset_of!(Header, reserved), arc_name_ex + 24);
        assert_eq!(offset_of!(Open, callback), 48);
        assert_eq!(offset_of!(Open, cmt_buf_w), 68);
        assert_eq!(size_of::<Open>(), 176);
    }
}
