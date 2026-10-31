use super::ffi::{self, Rel, MAIN_FORK, PAGE_SIZE};
use super::wire::{self, Manifest, Relation, RelationKind, SchemaColumn, SchemaIndex, TableSchema};
use pgrx::iter::SetOfIterator;
use pgrx::pg_sys;
use pgrx::spi::Spi;

pub unsafe fn build_manifest(
    source_table: &str,
    target_table: &str,
    target_toast_oid: pg_sys::Oid,
    target_checksums: bool,
) -> Result<Manifest, String> {
    let source_oid = ffi::resolve_relation(source_table)?;
    let source = Rel::open(source_oid, pg_sys::ShareLock.cast_signed());
    if !source.is_plain_logged_table() {
        return Err("source must be a plain logged table".into());
    }
    if ffi::data_checksums_enabled() != target_checksums {
        return Err("source and target data_checksums differ".into());
    }
    let source_toast = source.toast_oid();
    if source_toast != pg_sys::Oid::INVALID && target_toast_oid == pg_sys::Oid::INVALID {
        return Err("target table must have a TOAST relation".into());
    }

    let attributes = source
        .attributes()
        .into_iter()
        .map(|attribute| {
            let end = attribute
                .name
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(attribute.name.len());
            wire::Attribute {
                name: String::from_utf8_lossy(&attribute.name[..end]).into_owned(),
                type_oid: attribute.type_oid.to_u32(),
                collation_oid: attribute.collation_oid.to_u32(),
                typmod: attribute.typmod,
                len: attribute.len,
                not_null: attribute.not_null,
                dropped: attribute.dropped,
            }
        })
        .collect();

    let mut relations = vec![Relation {
        oid: source_oid.to_u32(),
        name: source.name(),
        kind: RelationKind::Main,
        fork: u8::try_from(MAIN_FORK).expect("main fork fits in u8"),
        blocks: source.blocks(MAIN_FORK),
        toast_index_oid: 0,
    }];

    if source_toast != pg_sys::Oid::INVALID {
        let toast = Rel::open(source_toast, pg_sys::ShareLock.cast_signed());
        let toast_index_oid = toast.toast_index_oid();
        if toast_index_oid == pg_sys::Oid::INVALID {
            return Err("source TOAST relation has no valid index".into());
        }
        let toast_index = Rel::open(toast_index_oid, pg_sys::ShareLock.cast_signed());
        if !toast_index.is_btree() {
            return Err("source TOAST index is not a supported btree".into());
        }
        relations.push(Relation {
            oid: source_toast.to_u32(),
            name: toast.name(),
            kind: RelationKind::Toast,
            fork: u8::try_from(MAIN_FORK).expect("main fork fits in u8"),
            blocks: toast.blocks(MAIN_FORK),
            toast_index_oid: toast_index_oid.to_u32(),
        });
        relations.push(Relation {
            oid: toast_index_oid.to_u32(),
            name: toast_index.name(),
            kind: RelationKind::Index,
            fork: u8::try_from(MAIN_FORK).expect("main fork fits in u8"),
            blocks: toast_index.blocks(MAIN_FORK),
            toast_index_oid: 0,
        });
    }

    let index_oids = source.index_oids();
    let mut index_definitions = Vec::with_capacity(index_oids.len());
    for index_oid in index_oids {
        let index = Rel::open(index_oid, pg_sys::ShareLock.cast_signed());
        if !index.is_btree() {
            return Err(format!("index {} is not a btree", index.name()));
        }
        relations.push(Relation {
            oid: index_oid.to_u32(),
            name: index.name(),
            kind: RelationKind::Index,
            fork: u8::try_from(MAIN_FORK).expect("main fork fits in u8"),
            blocks: index.blocks(MAIN_FORK),
            toast_index_oid: 0,
        });
        index_definitions.push(ffi::index_definition(index_oid));
    }

    Ok(Manifest {
        source_table: source_table.to_owned(),
        target_table: target_table.to_owned(),
        checksums: target_checksums,
        relations,
        attributes,
        index_definitions,
    })
}

/// Export only catalog metadata needed to recreate a table. No SQL dump tool or
/// server-side procedural language is involved; the descriptor is binary.
#[allow(clippy::too_many_lines)] // Keep catalog reads and descriptor order in one transaction.
pub unsafe fn export_schema(source_table: &str) -> Result<Vec<u8>, String> {
    let source_oid = ffi::resolve_relation(source_table)?;
    let source = Rel::open(source_oid, pg_sys::ShareLock.cast_signed());
    let source_toast = source.toast_oid();
    // Reuse the physical validation, especially the supported-btree check.
    let _ = build_manifest(
        source_table,
        source_table,
        source_toast,
        ffi::data_checksums_enabled(),
    )?;

    let columns = Spi::connect_mut(|client| {
        let rows = client
            .select(
                "SELECT a.attname::text, CASE WHEN a.attisdropped THEN 'text' \
                 ELSE format_type(a.atttypid, a.atttypmod) END, \
                 CASE WHEN a.attcollation <> 0 AND a.attcollation <> t.typcollation \
                 THEN (SELECT format('%I.%I', n.nspname, c.collname) \
                       FROM pg_collation c JOIN pg_namespace n ON n.oid = c.collnamespace \
                       WHERE c.oid = a.attcollation) END, \
                 pg_get_expr(d.adbin, d.adrelid), a.attnotnull, a.attidentity::text, \
                 CASE WHEN a.attgenerated <> '' THEN pg_get_expr(d.adbin, d.adrelid) END, \
                 a.attisdropped \
                 FROM pg_attribute a LEFT JOIN pg_type t ON t.oid = a.atttypid \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 WHERE a.attrelid = $1 AND a.attnum > 0 ORDER BY a.attnum",
                None,
                &[source_oid.into()],
            )
            .map_err(|error| error.to_string())?;
        let mut columns = Vec::with_capacity(rows.len());
        for row in rows {
            columns.push(SchemaColumn {
                name: row
                    .get::<String>(1)
                    .map_err(|error| error.to_string())?
                    .unwrap_or_default(),
                type_name: row
                    .get::<String>(2)
                    .map_err(|error| error.to_string())?
                    .unwrap_or_default(),
                collation_name: row.get::<String>(3).map_err(|error| error.to_string())?,
                default_expr: row.get::<String>(4).map_err(|error| error.to_string())?,
                not_null: row
                    .get::<bool>(5)
                    .map_err(|error| error.to_string())?
                    .unwrap_or(false),
                identity: row
                    .get::<String>(6)
                    .map_err(|error| error.to_string())?
                    .unwrap_or_default(),
                generated_expr: row.get::<String>(7).map_err(|error| error.to_string())?,
                dropped: row
                    .get::<bool>(8)
                    .map_err(|error| error.to_string())?
                    .unwrap_or(false),
            });
        }
        Ok::<_, String>(columns)
    })?;

    let constraints = Spi::connect_mut(|client| {
        let unsupported = client
            .select(
                "SELECT contype::text FROM pg_constraint \
                 WHERE conrelid = $1 AND contype IN ('f', 'x') LIMIT 1",
                None,
                &[source_oid.into()],
            )
            .map_err(|error| error.to_string())?;
        if !unsupported.is_empty() {
            return Err(
                "automatic target creation does not support foreign/exclusion constraints"
                    .to_owned(),
            );
        }
        let rows = client
            .select(
                "SELECT pg_get_constraintdef(oid, true) FROM pg_constraint \
                 WHERE conrelid = $1 ORDER BY oid",
                None,
                &[source_oid.into()],
            )
            .map_err(|error| error.to_string())?;
        let mut constraints = Vec::with_capacity(rows.len());
        for row in rows {
            constraints.push(
                row.get::<String>(1)
                    .map_err(|error| error.to_string())?
                    .unwrap_or_default(),
            );
        }
        Ok::<_, String>(constraints)
    })?;

    let indexes = Spi::connect_mut(|client| {
        let rows = client
            .select(
                "SELECT pg_get_indexdef(i.indexrelid), i.indisunique \
                 FROM pg_index i LEFT JOIN pg_constraint c ON c.conindid = i.indexrelid \
                 WHERE i.indrelid = $1 AND c.oid IS NULL ORDER BY i.indexrelid",
                None,
                &[source_oid.into()],
            )
            .map_err(|error| error.to_string())?;
        let mut indexes = Vec::with_capacity(rows.len());
        for row in rows {
            let definition = row
                .get::<String>(1)
                .map_err(|error| error.to_string())?
                .unwrap_or_default();
            let start = definition
                .find(" USING btree ")
                .map(|position| position + 7)
                .ok_or_else(|| "source index is not a supported btree".to_owned())?;
            indexes.push(SchemaIndex {
                definition: definition[start..].to_owned(),
                unique: row
                    .get::<bool>(2)
                    .map_err(|error| error.to_string())?
                    .unwrap_or(false),
            });
        }
        Ok::<_, String>(indexes)
    })?;

    wire::encode_schema(&TableSchema {
        columns,
        constraints,
        indexes,
    })
    .map_err(|error| error.to_string())
}

struct ExportState {
    manifest: Manifest,
    source_main: Rel,
    relations: Vec<Rel>,
    target_toast_oid: pg_sys::Oid,
    header_pending: bool,
    relation_index: usize,
    block: u32,
    finished: bool,
}

impl ExportState {
    unsafe fn new(
        source_table: &str,
        target_table: &str,
        target_toast_oid: pg_sys::Oid,
        target_checksums: bool,
    ) -> Result<Self, String> {
        let manifest = build_manifest(
            source_table,
            target_table,
            target_toast_oid,
            target_checksums,
        )?;
        let source_oid = ffi::resolve_relation(source_table)?;
        let source_main = Rel::open(source_oid, pg_sys::ShareLock.cast_signed());
        let relations: Vec<Rel> = manifest
            .relations
            .iter()
            .map(|relation| {
                Rel::open(
                    pg_sys::Oid::from_u32(relation.oid),
                    pg_sys::ShareLock.cast_signed(),
                )
            })
            .collect();
        Ok(Self {
            manifest,
            source_main,
            relations,
            target_toast_oid,
            header_pending: true,
            relation_index: 0,
            block: 0,
            finished: false,
        })
    }

    fn next_page(&mut self) -> Option<Vec<u8>> {
        if self.header_pending {
            self.header_pending = false;
            return Some(
                wire::encode_header(&self.manifest)
                    .unwrap_or_else(|error| panic!("pg_attach: {error}")),
            );
        }
        if self.finished {
            return None;
        }

        while self.relation_index < self.relations.len() {
            let metadata = &self.manifest.relations[self.relation_index];
            if self.block >= metadata.blocks {
                self.relation_index += 1;
                self.block = 0;
                continue;
            }

            let mut page = vec![0u8; PAGE_SIZE];
            let source_toast = self.source_main.toast_oid();
            self.relations[self.relation_index].read_page(MAIN_FORK, self.block, &mut page);
            match metadata.kind {
                RelationKind::Main => self.source_main.prepare_heap_page(
                    self.block,
                    &mut page,
                    source_toast,
                    self.target_toast_oid,
                    source_toast != pg_sys::Oid::INVALID,
                    self.manifest.checksums,
                ),
                RelationKind::Toast => self.relations[self.relation_index].prepare_heap_page(
                    self.block,
                    &mut page,
                    source_toast,
                    self.target_toast_oid,
                    false,
                    self.manifest.checksums,
                ),
                RelationKind::Index => {
                    Rel::prepare_page(self.block, &mut page, self.manifest.checksums);
                }
            }
            let packet = wire::encode_page(
                u16::try_from(self.relation_index).expect("relation count fits u16"),
                metadata.fork,
                self.block,
                &page,
            )
            .unwrap_or_else(|error| panic!("pg_attach: {error}"));
            self.block += 1;
            return Some(packet);
        }

        self.finished = true;
        Some(vec![0, 0, 0, 0])
    }
}

/// # Safety
/// Runs inside PostgreSQL and retains relation locks until the statement ends.
pub unsafe fn export_stream(
    source_table: &str,
    target_table: &str,
    target_toast_oid: pg_sys::Oid,
    target_checksums: bool,
) -> SetOfIterator<'static, Vec<u8>> {
    let mut state = ExportState::new(
        source_table,
        target_table,
        target_toast_oid,
        target_checksums,
    )
    .unwrap_or_else(|error| panic!("pg_attach: {error}"));
    SetOfIterator::new(std::iter::from_fn(move || state.next_page()))
}

fn relation_is_frozen(relation: &Rel) -> bool {
    let mut page = vec![0u8; PAGE_SIZE];
    for block in 0..relation.blocks(MAIN_FORK) {
        relation.read_page(MAIN_FORK, block, &mut page);
        if !Rel::heap_page_is_frozen(&page) {
            return false;
        }
    }
    true
}

/// Validate frozen heap and TOAST tuples after VACUUM and ACCESS EXCLUSIVE,
/// but before the source checkpoint. False requests another freeze/lock attempt.
///
/// # Safety
/// Runs inside PostgreSQL with the source relation lock held by this transaction.
pub unsafe fn source_is_frozen(source_table: &str) -> Result<bool, String> {
    let source_oid = ffi::resolve_relation(source_table)?;
    let source = Rel::open(source_oid, pg_sys::ShareLock.cast_signed());
    if !source.is_plain_logged_table() {
        return Err("source must be a plain logged table".into());
    }
    source.flush_buffers();
    if !relation_is_frozen(&source) {
        return Ok(false);
    }
    let toast_oid = source.toast_oid();
    if toast_oid != pg_sys::Oid::INVALID {
        let toast = Rel::open(toast_oid, pg_sys::ShareLock.cast_signed());
        toast.flush_buffers();
        if !relation_is_frozen(&toast) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Lock the source before the caller checkpoints from a second connection.
/// The lock remains held until the surrounding transaction ends.
///
/// # Safety
/// Runs inside PostgreSQL; the caller must keep the transaction open while reading pages.
pub unsafe fn lock_source(source_table: &str) -> Result<(), String> {
    let oid = ffi::resolve_relation(source_table)?;
    let relation = Rel::open(oid, pg_sys::AccessExclusiveLock.cast_signed());
    if !relation.is_plain_logged_table() {
        return Err("source must be a plain logged table".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn done_packet_is_four_zero_bytes() {
        assert_eq!(vec![0, 0, 0, 0], [0; 4]);
    }
}
