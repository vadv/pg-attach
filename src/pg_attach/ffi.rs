use super::wire;
use pgrx::{pg_sys, spi::Spi};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

pub const PAGE_SIZE: usize = pg_sys::BLCKSZ as usize;
pub const MAIN_FORK: pg_sys::ForkNumber::Type = pg_sys::ForkNumber::MAIN_FORKNUM;
pub const FSM_FORK: pg_sys::ForkNumber::Type = pg_sys::ForkNumber::FSM_FORKNUM;
pub const VM_FORK: pg_sys::ForkNumber::Type = pg_sys::ForkNumber::VISIBILITYMAP_FORKNUM;

/// Map the wire fork byte back to a PostgreSQL fork number, rejecting unknown
/// forks so old or corrupt manifests fail closed.
pub fn fork_from_wire(fork: u8) -> Result<pg_sys::ForkNumber::Type, String> {
    for candidate in [MAIN_FORK, FSM_FORK, VM_FORK] {
        if fork == u8::try_from(candidate).expect("fork number fits u8") {
            return Ok(candidate);
        }
    }
    Err(format!("unsupported fork number {fork}"))
}

/// Connection string pointing back at this backend's own postmaster, used to
/// run VACUUM FREEZE outside the import transaction.
///
/// # Safety
/// Reads server GUCs and OIDs; runs inside PostgreSQL.
pub unsafe fn self_conninfo() -> Result<String, String> {
    let sockets = CStr::from_ptr(pg_sys::GetConfigOption(
        c"unix_socket_directories".as_ptr(),
        false,
        true,
    ))
    .to_string_lossy();
    let socket = sockets.split(',').next().unwrap_or_default().trim();
    if socket.is_empty() {
        return Err("unix_socket_directories is empty".into());
    }
    let dbname = CStr::from_ptr(pg_sys::get_database_name(pg_sys::MyDatabaseId))
        .to_string_lossy()
        .into_owned();
    let user = CStr::from_ptr(pg_sys::GetUserNameFromId(pg_sys::GetUserId(), true))
        .to_string_lossy()
        .into_owned();
    let port = unsafe { *std::ptr::addr_of!(pg_sys::PostPortNumber) };
    let quote = |value: &str| format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"));
    Ok(format!(
        "host={socket} port={port} dbname={} user={}",
        quote(&dbname),
        quote(&user),
    ))
}

#[allow(clippy::struct_excessive_bools)] // Matches the C catalog representation exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawAttribute {
    pub name: [u8; 64],
    pub typmod: i32,
    pub len: i16,
    pub align: u8,
    pub by_val: bool,
    pub storage: u8,
    pub has_missing: bool,
    pub generated: u8,
    pub not_null: bool,
    pub dropped: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawBtreeMetadata {
    version: u32,
    all_equal_image: bool,
    expected_all_equal_image: bool,
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
    fn pg_attach_verify_page(
        page: *mut c_char,
        block: pg_sys::BlockNumber,
        checksums: bool,
    ) -> bool;
    fn pg_attach_heap_page_is_frozen(page: *const c_char) -> bool;
    fn pg_attach_prepare_heap_page(
        page: *mut c_char,
        rel: pg_sys::Relation,
        block: pg_sys::BlockNumber,
        source_toast: pg_sys::Oid,
        target_toast: pg_sys::Oid,
        patch_toast: bool,
        target_lz4_supported: bool,
        checksums: bool,
    );
    fn pg_attach_verify_btree_metapage(page: *mut c_char, all_equal_image: bool) -> bool;
    fn pg_attach_btree_metadata(rel: pg_sys::Relation, metadata: *mut RawBtreeMetadata);
    fn pg_attach_max_toast_chunk_size() -> c_int;
    fn pg_attach_max_identifier_length() -> c_int;
    fn pg_attach_checksum_algorithm_version() -> c_int;
    fn pg_attach_lz4_supported() -> bool;
    fn pg_attach_toast_index_oid(rel: pg_sys::Relation) -> pg_sys::Oid;
    fn pg_attach_index_oids(rel: pg_sys::Relation, oids: *mut *mut pg_sys::Oid) -> c_int;
    fn pg_attach_index_definition(index_oid: pg_sys::Oid) -> *const c_char;
    fn pg_attach_attributes(rel: pg_sys::Relation, attrs: *mut RawAttribute);
    fn pg_attach_attribute_count(rel: pg_sys::Relation) -> c_int;
    fn pg_attach_blocks_safe(
        rel: pg_sys::Relation,
        fork: pg_sys::ForkNumber::Type,
    ) -> pg_sys::BlockNumber;
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

pub struct Rel(pg_sys::Relation, pg_sys::Oid);

// Stack-local: reuse one VM pin per relation and release it before returning to PostgreSQL.
pub struct VisibilityMap<'a> {
    relation: &'a Rel,
    buffer: pg_sys::Buffer,
}

impl VisibilityMap<'_> {
    pub fn all_frozen(&mut self, block: u32) -> bool {
        pg_sys::check_for_interrupts!();
        let status = unsafe {
            pg_sys::visibilitymap_get_status(self.relation.0, block, &raw mut self.buffer)
        };
        u32::from(status) & pg_sys::VISIBILITYMAP_ALL_FROZEN != 0
    }
}

impl Drop for VisibilityMap<'_> {
    fn drop(&mut self) {
        if self.buffer != pg_sys::InvalidBuffer.cast_signed() {
            unsafe { pg_sys::ReleaseBuffer(self.buffer) }
        }
    }
}

impl Rel {
    /// Opens a relation and retains its lock until transaction end on Drop.
    pub unsafe fn open(oid: pg_sys::Oid, lock: pg_sys::LOCKMODE) -> Self {
        Self(pg_sys::relation_open(oid, lock), oid)
    }

    pub fn as_ptr(&self) -> pg_sys::Relation {
        self.0
    }

    pub fn oid(&self) -> pg_sys::Oid {
        self.1
    }

    pub fn blocks(&self, fork: pg_sys::ForkNumber::Type) -> u32 {
        unsafe { pg_sys::RelationGetNumberOfBlocksInFork(self.0, fork) }
    }

    /// Size of an optional fork; unlike [`blocks`] this returns 0 instead of
    /// raising when the fork file does not exist (indexes and fresh tables
    /// have no FSM/VM files).
    pub fn blocks_if_exists(&self, fork: pg_sys::ForkNumber::Type) -> u32 {
        unsafe { pg_attach_blocks_safe(self.0, fork) }
    }

    pub fn toast_oid(&self) -> pg_sys::Oid {
        unsafe { (*(*self.0).rd_rel).reltoastrelid }
    }

    pub fn toast_index_oid(&self) -> pg_sys::Oid {
        unsafe { guard(|| pg_attach_toast_index_oid(self.0)) }
    }

    #[allow(clippy::cast_possible_wrap)]
    pub fn is_plain_logged_table(&self) -> bool {
        unsafe {
            let rel = &*(*self.0).rd_rel;
            rel.relkind == pg_sys::RELKIND_RELATION as std::os::raw::c_char
                && rel.relpersistence == pg_sys::RELPERSISTENCE_PERMANENT as std::os::raw::c_char
        }
    }

    #[allow(clippy::cast_possible_wrap)]
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
                && relation.rd_indexprs.is_null()
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

    fn raw_attributes(&self) -> Vec<RawAttribute> {
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

    #[allow(clippy::too_many_lines)] // Keep version-safe catalog identity SQL beside its row mapping.
    pub fn attributes(&self) -> Result<Vec<wire::Attribute>, String> {
        let raw = self.raw_attributes();
        let identities = Spi::connect(|client| {
            let rows = client.select(
                "SELECT CASE WHEN t.oid IS NULL OR tn.oid IS NULL THEN '' \
                      ELSE format('%I.%I', tn.nspname, t.typname) END, \
                 CASE WHEN a.attcollation = 0 OR (c.oid IS NULL AND dc.oid IS NULL) THEN NULL \
                      ELSE format('%I.%I|%s|%s|%s|%s|%s', \
                       coalesce(dcn.nspname, cn.nspname), coalesce(dc.collname, c.collname), \
                       coalesce(dc.collprovider, c.collprovider), \
                       coalesce(row_to_json(dc)->>'colllocale', row_to_json(dc)->>'colliculocale', \
                                dc.collcollate, row_to_json(c)->>'colllocale', \
                                row_to_json(c)->>'colliculocale', c.collcollate), \
                       coalesce(dc.collctype, c.collctype, ''), \
                       coalesce(dc.collisdeterministic, c.collisdeterministic), \
                       coalesce(dc.collencoding, c.collencoding)) END, \
                 CASE WHEN a.attcollation = 0 THEN '' \
                      ELSE coalesce(c.collversion, dc.collversion, '') END, \
                 CASE WHEN a.attcollation = 0 THEN '' ELSE coalesce(\
                      pg_collation_actual_version(c.oid), \
                      pg_collation_actual_version(dc.oid), '') END, \
                 EXISTS ( \
                   SELECT 1 FROM pg_database db \
                   WHERE db.datname = current_database() AND c.oid = 100 \
                     AND coalesce(row_to_json(db)->>'daticulocale', \
                                  row_to_json(db)->>'datlocale') IS NOT NULL \
                     AND NOT EXISTS ( \
                       SELECT 1 FROM pg_collation dc \
                       WHERE dc.collprovider = 'i' \
                         AND dc.collname = coalesce(row_to_json(db)->>'daticulocale', \
                                                    row_to_json(db)->>'datlocale') \
                         AND coalesce(row_to_json(dc)->>'colllocale', \
                                      row_to_json(dc)->>'colliculocale', dc.collcollate) = \
                             coalesce(row_to_json(db)->>'daticulocale', \
                                      row_to_json(db)->>'datlocale') \
                         AND dc.collencoding IN (-1, pg_char_to_encoding(current_setting('server_encoding'))) \
                     ) \
                 ) AS default_identity_missing \
                 FROM pg_attribute a \
                 LEFT JOIN pg_type t ON t.oid = a.atttypid \
                 LEFT JOIN pg_namespace tn ON tn.oid = t.typnamespace \
                 LEFT JOIN pg_collation c ON c.oid = a.attcollation \
                 LEFT JOIN pg_namespace cn ON cn.oid = c.collnamespace \
                 LEFT JOIN LATERAL ( \
                     SELECT dc.* FROM pg_database d \
                     JOIN pg_collation dc ON \
                       (dc.collprovider = 'c' AND dc.collname = d.datcollate) OR \
                       (dc.collprovider = 'i' AND dc.collname = coalesce(\
                         row_to_json(d)->>'daticulocale', row_to_json(d)->>'datlocale') \
                         AND coalesce(row_to_json(dc)->>'colllocale', \
                                      row_to_json(dc)->>'colliculocale', dc.collcollate) = \
                             coalesce(row_to_json(d)->>'daticulocale', \
                                      row_to_json(d)->>'datlocale')) \
                     WHERE d.datname = current_database() AND c.oid = 100 \
                       AND dc.collencoding IN (-1, pg_char_to_encoding(current_setting('server_encoding'))) \
                     ORDER BY (dc.collprovider = 'i') DESC, dc.oid LIMIT 1 \
                 ) dc ON true \
                 LEFT JOIN pg_namespace dcn ON dcn.oid = dc.collnamespace \
                 WHERE a.attrelid = $1 AND a.attnum > 0 ORDER BY a.attnum",
                None,
                &[self.1.into()],
            )?;
            let mut identities = Vec::with_capacity(rows.len());
            for row in rows {
                identities.push((
                    row.get::<String>(1)?.unwrap_or_default(),
                    row.get::<String>(2)?,
                    row.get::<String>(3)?.unwrap_or_default(),
                    row.get::<String>(4)?.unwrap_or_default(),
                    row.get::<bool>(5)?.unwrap_or(true),
                ));
            }
            Ok::<_, pgrx::spi::Error>(identities)
        })
        .map_err(|error| error.to_string())?;
        if identities.iter().any(|identity| identity.4) {
            return Err(
                "database default collation identity cannot be proven without a concrete matching pg_collation row"
                    .into(),
            );
        }
        if raw.len() != identities.len() {
            return Err("attribute catalog metadata changed during transfer".into());
        }
        Ok(raw
            .into_iter()
            .zip(identities)
            .map(|(attribute, identity)| {
                let end = attribute
                    .name
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(attribute.name.len());
                wire::Attribute {
                    name: String::from_utf8_lossy(&attribute.name[..end]).into_owned(),
                    type_name: identity.0,
                    collation_name: identity.1,
                    collation_version: identity.2,
                    collation_actual_version: identity.3,
                    typmod: attribute.typmod,
                    len: attribute.len,
                    align: attribute.align,
                    by_val: attribute.by_val,
                    storage: attribute.storage,
                    has_missing: attribute.has_missing,
                    generated: attribute.generated,
                    not_null: attribute.not_null,
                    dropped: attribute.dropped,
                }
            })
            .collect())
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

    pub fn verify_page(block: u32, page: &mut [u8], checksums: bool) -> bool {
        assert_eq!(page.len(), PAGE_SIZE);
        unsafe { guard(|| pg_attach_verify_page(page.as_mut_ptr().cast(), block, checksums)) }
    }

    pub fn verify_btree_metapage(page: &mut [u8], all_equal_image: bool) -> bool {
        assert_eq!(page.len(), PAGE_SIZE);
        unsafe {
            guard(|| pg_attach_verify_btree_metapage(page.as_mut_ptr().cast(), all_equal_image))
        }
    }

    pub fn visibility_map(&self) -> VisibilityMap<'_> {
        VisibilityMap {
            relation: self,
            buffer: pg_sys::InvalidBuffer.cast_signed(),
        }
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
        target_lz4_supported: bool,
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
                    target_lz4_supported,
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

#[allow(clippy::too_many_lines)] // Keep index catalog identity and default-collation checks together.
pub fn index_metadata(oid: pg_sys::Oid) -> Result<wire::IndexMetadata, String> {
    let unsupported = Spi::get_one_with_args::<bool>(
        "SELECT indexprs IS NOT NULL OR indpred IS NOT NULL FROM pg_index WHERE indexrelid = $1",
        &[oid.into()],
    )
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "index catalog metadata is missing".to_owned())?;
    if unsupported {
        return Err("expression and partial indexes are not supported".into());
    }
    let custom_opclass = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (SELECT 1 FROM pg_index i \
         JOIN LATERAL unnest(i.indclass) AS u(oid) ON true \
         JOIN pg_opclass o ON o.oid = u.oid \
         JOIN pg_namespace n ON n.oid = o.opcnamespace \
         WHERE i.indexrelid = $1 AND n.nspname <> 'pg_catalog')",
        &[oid.into()],
    )
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "index opclass metadata is missing".to_owned())?;
    if custom_opclass {
        return Err(
            "custom opclasses are not supported; only pg_catalog btree opclasses are allowed"
                .into(),
        );
    }
    let default_identity_missing = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS ( \
           SELECT 1 FROM pg_index i \
           JOIN LATERAL unnest(i.indcollation) AS u(oid) ON true \
           JOIN pg_collation c ON c.oid = u.oid \
           JOIN pg_database db ON db.datname = current_database() \
           WHERE i.indexrelid = $1 AND c.oid = 100 \
             AND coalesce(row_to_json(db)->>'daticulocale', \
                          row_to_json(db)->>'datlocale') IS NOT NULL \
             AND NOT EXISTS ( \
               SELECT 1 FROM pg_collation dc \
               WHERE dc.collprovider = 'i' \
                 AND dc.collname = coalesce(row_to_json(db)->>'daticulocale', \
                                            row_to_json(db)->>'datlocale') \
                 AND coalesce(row_to_json(dc)->>'colllocale', \
                              row_to_json(dc)->>'colliculocale', dc.collcollate) = \
                     coalesce(row_to_json(db)->>'daticulocale', \
                              row_to_json(db)->>'datlocale') \
                 AND dc.collencoding IN (-1, pg_char_to_encoding(current_setting('server_encoding'))) \
             ) \
         )",
        &[oid.into()],
    )
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "index default-collation metadata is missing".to_owned())?;
    if default_identity_missing {
        return Err(
            "database default collation identity cannot be proven without a concrete matching pg_collation row"
                .into(),
        );
    }
    let shape = Spi::get_one_with_args::<String>(
        "SELECT format('%s|%s|%s|%s|%s|%s|%s|%s', \
           i.indnkeyatts, i.indnatts, i.indisunique, i.indnullsnotdistinct, \
           i.indkey, i.indoption, \
           coalesce((SELECT string_agg(format('%I.%I', n.nspname, o.opcname), ',' ORDER BY u.ord) \
             FROM unnest(i.indclass) WITH ORDINALITY AS u(oid, ord) \
             JOIN pg_opclass o ON o.oid = u.oid JOIN pg_namespace n ON n.oid = o.opcnamespace), ''), \
           coalesce((SELECT string_agg(format('%I.%I|%s|%s|%s|%s|%s|%s|%s', \
             coalesce(dcn.nspname, n.nspname), coalesce(dc.collname, c.collname), \
             coalesce(dc.collprovider, c.collprovider), \
             coalesce(row_to_json(dc)->>'colllocale', row_to_json(dc)->>'colliculocale', \
                      dc.collcollate, row_to_json(c)->>'colllocale', \
                      row_to_json(c)->>'colliculocale', c.collcollate), \
             coalesce(dc.collctype, c.collctype, ''), \
             coalesce(dc.collisdeterministic, c.collisdeterministic), \
             coalesce(dc.collencoding, c.collencoding), \
             coalesce(dc.collversion, c.collversion, ''), \
             coalesce(pg_collation_actual_version(dc.oid), \
                      pg_collation_actual_version(c.oid), '')), ',' ORDER BY u.ord) \
             FROM unnest(i.indcollation) WITH ORDINALITY AS u(oid, ord) \
             JOIN pg_collation c ON c.oid = u.oid \
             JOIN pg_namespace n ON n.oid = c.collnamespace \
             LEFT JOIN LATERAL ( \
               SELECT dc.* FROM pg_database d \
               JOIN pg_collation dc ON \
                 (dc.collprovider = 'c' AND dc.collname = d.datcollate) OR \
                 (dc.collprovider = 'i' AND dc.collname = coalesce(\
                   row_to_json(d)->>'daticulocale', row_to_json(d)->>'datlocale') \
                   AND coalesce(row_to_json(dc)->>'colllocale', \
                                row_to_json(dc)->>'colliculocale', dc.collcollate) = \
                       coalesce(row_to_json(d)->>'daticulocale', \
                                row_to_json(d)->>'datlocale')) \
               WHERE d.datname = current_database() AND c.oid = 100 \
                 AND dc.collencoding IN (-1, pg_char_to_encoding(current_setting('server_encoding'))) \
               ORDER BY (dc.collprovider = 'i') DESC, dc.oid LIMIT 1 \
             ) dc ON true \
             LEFT JOIN pg_namespace dcn ON dcn.oid = dc.collnamespace), '')) \
         FROM pg_index i WHERE i.indexrelid = $1",
        &[oid.into()],
    )
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "index catalog metadata is missing".to_owned())?;
    let rel = unsafe { Rel::open(oid, pg_sys::ShareLock.cast_signed()) };
    let mut raw = RawBtreeMetadata::default();
    unsafe { guard(|| pg_attach_btree_metadata(rel.0, &raw mut raw)) };
    Ok(wire::IndexMetadata {
        shape,
        btree_version: raw.version,
        all_equal_image: raw.all_equal_image,
        expected_all_equal_image: raw.expected_all_equal_image,
    })
}

pub fn max_toast_chunk_size() -> i32 {
    unsafe { guard(|| pg_attach_max_toast_chunk_size()) }
}

pub fn max_identifier_length() -> i32 {
    unsafe { guard(|| pg_attach_max_identifier_length()) }
}

pub fn checksum_algorithm_version() -> i32 {
    unsafe { guard(|| pg_attach_checksum_algorithm_version()) }
}

pub fn lz4_supported() -> bool {
    unsafe { guard(|| pg_attach_lz4_supported()) }
}
