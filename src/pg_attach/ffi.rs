use pgrx::pg_sys;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

pub const PAGE_SIZE: usize = pg_sys::BLCKSZ as usize;
pub const MAIN_FORK: pg_sys::ForkNumber::Type = pg_sys::ForkNumber::MAIN_FORKNUM;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawAttribute {
    pub name: [u8; 64],
    pub type_oid: pg_sys::Oid,
    pub collation_oid: pg_sys::Oid,
    pub typmod: i32,
    pub len: i16,
    pub not_null: bool,
    pub dropped: bool,
}

unsafe extern "C" {
    fn pg_attach_resolve_relation(name: *const c_char) -> pg_sys::Oid;
    fn pg_attach_flush_relation(rel: pg_sys::Relation);
    fn pg_attach_read_page(
        rel: pg_sys::Relation,
        fork: pg_sys::ForkNumber::Type,
        block: pg_sys::BlockNumber,
        dst: *mut c_char,
    );
    fn pg_attach_prepare_page(page: *mut c_char, block: pg_sys::BlockNumber, checksums: bool);
    fn pg_attach_heap_page_is_frozen(page: *const c_char) -> bool;
    fn pg_attach_prepare_heap_page(
        page: *mut c_char,
        rel: pg_sys::Relation,
        block: pg_sys::BlockNumber,
        source_toast: pg_sys::Oid,
        target_toast: pg_sys::Oid,
        patch_toast: bool,
        checksums: bool,
    );
    fn pg_attach_toast_index_oid(rel: pg_sys::Relation) -> pg_sys::Oid;
    fn pg_attach_index_oids(rel: pg_sys::Relation, oids: *mut *mut pg_sys::Oid) -> c_int;
    fn pg_attach_index_definition(index_oid: pg_sys::Oid) -> *const c_char;
    fn pg_attach_attributes(rel: pg_sys::Relation, attrs: *mut RawAttribute);
    fn pg_attach_attribute_count(rel: pg_sys::Relation) -> c_int;
    fn pg_attach_write_page(
        rel: pg_sys::Relation,
        fork: pg_sys::ForkNumber::Type,
        block: pg_sys::BlockNumber,
        page: *const c_char,
    );
    fn pg_attach_log_relation(rel: pg_sys::Relation, fork: pg_sys::ForkNumber::Type);
    fn pg_attach_sync_relation(rel: pg_sys::Relation, fork: pg_sys::ForkNumber::Type);
}

#[inline]
unsafe fn guard<T>(f: impl FnOnce() -> T) -> T {
    // Our hand-written C shim can raise ERROR; pgrx must catch the longjmp.
    unsafe { pg_sys::ffi::pg_guard_ffi_boundary(f) }
}

pub fn resolve_relation(name: &str) -> Result<pg_sys::Oid, String> {
    let name = std::ffi::CString::new(name).map_err(|_| "relation name contains NUL".to_owned())?;
    let oid = unsafe { guard(|| pg_attach_resolve_relation(name.as_ptr())) };
    if oid == pg_sys::Oid::INVALID {
        Err("relation does not exist".to_owned())
    } else {
        Ok(oid)
    }
}

pub struct Rel(pg_sys::Relation);

impl Rel {
    /// Opens a relation and retains its lock until transaction end on Drop.
    pub unsafe fn open(oid: pg_sys::Oid, lock: pg_sys::LOCKMODE) -> Self {
        Self(pg_sys::relation_open(oid, lock))
    }

    pub fn as_ptr(&self) -> pg_sys::Relation {
        self.0
    }

    pub fn blocks(&self, fork: pg_sys::ForkNumber::Type) -> u32 {
        unsafe { pg_sys::RelationGetNumberOfBlocksInFork(self.0, fork) }
    }

    pub fn toast_oid(&self) -> pg_sys::Oid {
        unsafe { (*(*self.0).rd_rel).reltoastrelid }
    }

    pub fn toast_index_oid(&self) -> pg_sys::Oid {
        unsafe { guard(|| pg_attach_toast_index_oid(self.0)) }
    }

    #[allow(clippy::cast_possible_wrap)] // PostgreSQL constants are ASCII on either c_char signedness.
    pub fn is_plain_logged_table(&self) -> bool {
        unsafe {
            let rel = &*(*self.0).rd_rel;
            rel.relkind == pg_sys::RELKIND_RELATION as std::os::raw::c_char
                && rel.relpersistence == pg_sys::RELPERSISTENCE_PERMANENT as std::os::raw::c_char
        }
    }

    #[allow(clippy::cast_possible_wrap)] // PostgreSQL constants are ASCII on either c_char signedness.
    pub fn is_btree(&self) -> bool {
        unsafe {
            let relation = &*self.0;
            let rel = &*relation.rd_rel;
            rel.relkind == pg_sys::RELKIND_INDEX as std::os::raw::c_char
                && rel.relam == pg_sys::BTREE_AM_OID
                && !relation.rd_index.is_null()
                && (*relation.rd_index).indisvalid
                && (*relation.rd_index).indisready
                && relation.rd_indpred.is_null()
                && !(*relation.rd_index).indisexclusion
        }
    }

    pub fn name(&self) -> String {
        unsafe { CStr::from_ptr((*(*self.0).rd_rel).relname.data.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    pub fn index_oids(&self) -> Vec<pg_sys::Oid> {
        let mut ptr = std::ptr::null_mut();
        let count =
            usize::try_from(unsafe { guard(|| pg_attach_index_oids(self.0, &raw mut ptr)) })
                .expect("index count is nonnegative");
        if count == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(ptr, count) }.to_vec()
        }
    }

    pub fn attributes(&self) -> Vec<RawAttribute> {
        let count = usize::try_from(unsafe { guard(|| pg_attach_attribute_count(self.0)) })
            .expect("attribute count is nonnegative");
        let mut attrs = Vec::with_capacity(count);
        unsafe {
            let ptr = attrs.as_mut_ptr();
            guard(|| pg_attach_attributes(self.0, ptr));
            attrs.set_len(count);
        }
        attrs
    }

    pub fn flush_buffers(&self) {
        unsafe { guard(|| pg_attach_flush_relation(self.0)) }
    }

    pub fn read_page(&self, fork: pg_sys::ForkNumber::Type, block: u32, dst: &mut [u8]) {
        assert_eq!(dst.len(), PAGE_SIZE);
        unsafe { guard(|| pg_attach_read_page(self.0, fork, block, dst.as_mut_ptr().cast())) }
    }

    pub fn prepare_page(block: u32, page: &mut [u8], checksums: bool) {
        unsafe { guard(|| pg_attach_prepare_page(page.as_mut_ptr().cast(), block, checksums)) }
    }

    pub fn heap_page_is_frozen(page: &[u8]) -> bool {
        assert_eq!(page.len(), PAGE_SIZE);
        unsafe { guard(|| pg_attach_heap_page_is_frozen(page.as_ptr().cast())) }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_heap_page(
        &self,
        block: u32,
        page: &mut [u8],
        source_toast: pg_sys::Oid,
        target_toast: pg_sys::Oid,
        patch_toast: bool,
        checksums: bool,
    ) {
        unsafe {
            guard(|| {
                pg_attach_prepare_heap_page(
                    page.as_mut_ptr().cast(),
                    self.0,
                    block,
                    source_toast,
                    target_toast,
                    patch_toast,
                    checksums,
                );
            });
        }
    }

    pub fn write_page(&self, fork: pg_sys::ForkNumber::Type, block: u32, page: &[u8]) {
        assert_eq!(page.len(), PAGE_SIZE);
        unsafe { guard(|| pg_attach_write_page(self.0, fork, block, page.as_ptr().cast())) }
    }

    pub fn log_relation(&self, fork: pg_sys::ForkNumber::Type) {
        unsafe { guard(|| pg_attach_log_relation(self.0, fork)) }
    }

    pub fn sync_relation(&self, fork: pg_sys::ForkNumber::Type) {
        unsafe { guard(|| pg_attach_sync_relation(self.0, fork)) }
    }
}

impl Drop for Rel {
    fn drop(&mut self) {
        unsafe {
            pg_sys::relation_close(self.0, pg_sys::NoLock.cast_signed());
        }
    }
}

pub fn data_checksums_enabled() -> bool {
    unsafe { pg_sys::DataChecksumsEnabled() }
}

pub fn index_definition(oid: pg_sys::Oid) -> String {
    let definition = unsafe { guard(|| pg_attach_index_definition(oid)) };
    unsafe { CStr::from_ptr(definition) }
        .to_string_lossy()
        .into_owned()
}
