use pgrx::prelude::*;

pgrx::pg_module_magic!();

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
