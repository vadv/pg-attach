use pgrx::prelude::*;

pgrx::pg_module_magic!();

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};

/// Debug-only corruption hook for the BDD suite: flips one byte of the chosen
/// heap block before page verification. Negative values disable the hook.
static DEBUG_CORRUPT_PAGE: GucSetting<i32> = GucSetting::<i32>::new(-1);
static DEBUG_CORRUPT_OFFSET: GucSetting<i32> = GucSetting::<i32>::new(-1);

#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
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
    use super::{name, pg_extern, pg_sys, SetOfIterator, TableIterator};
    use pgrx::spi::Spi;

    mod exporter;
    mod ffi;
    mod importer;
    mod source_client;
    mod wire;

    /// Return physical and extension compatibility metadata for this server.
    ///
    /// # Panics
    /// Panics if the database encoding or installed extension version cannot be read.
    #[must_use]
    #[pg_extern]
    #[allow(clippy::type_complexity)] // Keep pgrx SQL column names beside their Rust types.
    pub fn pg_attach_server_info() -> TableIterator<
        'static,
        (
            name!(server_version_num, i32),
            name!(catversion, i32),
            name!(block_size, i32),
            name!(max_align, i32),
            name!(endianness, &'static str),
            name!(server_encoding, String),
            name!(data_checksums, bool),
            name!(extension_version, String),
        ),
    > {
        // Read-only SPI avoids assigning an XID before the source VACUUM.
        let server_encoding = Spi::connect(|client| {
            client
                .select("SELECT current_setting('server_encoding')", Some(1), &[])?
                .first()
                .get_one::<String>()
        })
        .unwrap_or_else(|error| panic!("pg_attach: cannot read server encoding: {error}"))
        .unwrap_or_else(|| panic!("pg_attach: server encoding is NULL"));
        let extension_version = Spi::connect(|client| {
            client
                .select(
                    "SELECT extversion FROM pg_catalog.pg_extension WHERE extname = 'pg_attach'",
                    Some(1),
                    &[],
                )?
                .first()
                .get_one::<String>()
        })
        .unwrap_or_else(|error| panic!("pg_attach: cannot read extension version: {error}"))
        .unwrap_or_else(|| panic!("pg_attach: extension version is missing"));
        let endianness = if cfg!(target_endian = "little") {
            "little"
        } else {
            "big"
        };

        TableIterator::new(std::iter::once((
            i32::try_from(pg_sys::PG_VERSION_NUM).expect("server version fits i32"),
            i32::try_from(pg_sys::CATALOG_VERSION_NO).expect("catalog version fits i32"),
            i32::try_from(pg_sys::BLCKSZ).expect("block size fits i32"),
            i32::try_from(pg_sys::MAXIMUM_ALIGNOF).expect("alignment fits i32"),
            endianness,
            server_encoding,
            unsafe { pg_sys::DataChecksumsEnabled() },
            extension_version,
        )))
    }

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

    /// Clone a table inside the current database, freezing it first.
    /// Emits a NOTICE for the automatic VACUUM (FREEZE) step.
    ///
    /// # Panics
    /// Panics if freezing, target creation or page installation fails.
    #[pg_extern]
    pub fn pg_attach_import_local(source_table: &str, target_table: &str, generate_wal: bool) {
        unsafe {
            importer::import_local(source_table, target_table, generate_wal)
                .unwrap_or_else(|error| panic!("pg_attach: {error}"));
        }
    }
}
