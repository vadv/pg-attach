use pgrx::prelude::*;

pgrx::pg_module_magic!();

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};

/// Debug-only corruption hook for the BDD suite: flips one byte of the chosen
/// heap block before page verification. Negative values disable the hook.
static DEBUG_CORRUPT_PAGE: GucSetting<i32> = GucSetting::new(-1);
static DEBUG_CORRUPT_OFFSET: GucSetting<i32> = GucSetting::new(-1);

#[pg_guard]
pub extern "C" fn _PG_init() {
    GucRegistry::define_int_guc(
        c"pg_attach.debug_corrupt_page",
        c"Heap block to corrupt during import (debug)",
        c"Flips one byte of this heap relation block before page verification. -1 disables.",
        &DEBUG_CORRUPT_PAGE,
        -1,
        i32::MAX,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pg_attach.debug_corrupt_offset",
        c"Byte offset to corrupt during import (debug)",
        c"Flips the byte at this offset of the block chosen by pg_attach.debug_corrupt_page.",
        &DEBUG_CORRUPT_OFFSET,
        -1,
        8191,
        GucContext::Userset,
        GucFlags::default(),
    );
}

pub mod pg_attach {
    use super::{pg_extern, pg_sys, SetOfIterator};

    mod exporter;
    mod ffi;
    mod importer;
    mod source_client;
    mod wire;

    /// Export one frozen table as a value-per-call COPY bytea stream.
    #[must_use]
    #[pg_extern]
    pub fn pg_attach_export_stream(
        source_table: &str,
        target_table: &str,
        target_toast_oid: pg_sys::Oid,
        target_checksums: bool,
    ) -> SetOfIterator<'static, Vec<u8>> {
        unsafe {
            exporter::export_stream(
                source_table,
                target_table,
                target_toast_oid,
                target_checksums,
            )
        }
    }

    /// Lock a source table for a direct-read transfer in the current transaction.
    ///
    /// # Panics
    /// Panics if the relation cannot be resolved or is not a plain logged table.
    #[pg_extern]
    pub fn pg_attach_lock_source(source_table: &str) {
        unsafe { exporter::lock_source(source_table) }
            .unwrap_or_else(|error| panic!("pg_attach: {error}"));
    }

    /// Check heap and TOAST tuples while ACCESS EXCLUSIVE is held, before checkpoint.
    ///
    /// # Panics
    /// Panics if the relation cannot be resolved or a page cannot be read.
    #[must_use]
    #[pg_extern]
    pub fn pg_attach_source_is_frozen(source_table: &str) -> bool {
        unsafe { exporter::source_is_frozen(source_table) }
            .unwrap_or_else(|error| panic!("pg_attach: {error}"))
    }

    /// Export a compact, target-name-independent schema descriptor.
    ///
    /// # Panics
    /// Panics if the source catalog cannot be read or the descriptor cannot be encoded.
    #[must_use]
    #[pg_extern]
    pub fn pg_attach_export_schema(source_table: &str) -> Vec<u8> {
        unsafe { exporter::export_schema(source_table) }
            .unwrap_or_else(|error| panic!("pg_attach: {error}"))
    }

    /// Pull a table from another PostgreSQL instance and install its pages locally.
    ///
    /// # Panics
    /// Panics if validation, target creation, streaming or page installation fails.
    #[pg_extern]
    pub fn pg_attach_import(
        source_conninfo: &str,
        source_table: &str,
        target_table: &str,
        generate_wal: bool,
    ) {
        unsafe {
            importer::import(source_conninfo, source_table, target_table, generate_wal)
                .unwrap_or_else(|error| panic!("pg_attach: {error}"));
        }
    }
}
