use super::ffi::{self, Rel, MAIN_FORK};
use super::source_client::SourceClient;
use super::wire::{self, Manifest, RelationKind, TableSchema};
use pgrx::pg_sys;
use pgrx::spi::Spi;
use std::io::Cursor;

struct ImportState {
    manifest: Manifest,
    targets: Vec<Rel>,
    next_blocks: Vec<u32>,
    done: bool,
}

fn attributes(rel: &Rel) -> Vec<wire::Attribute> {
    rel.attributes()
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
        .collect()
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('\"', "\"\""))
}

fn quote_qualified_relation(value: &str) -> Result<(String, Vec<String>), String> {
    let parts =
        Spi::get_one_with_args::<Vec<String>>("SELECT pg_catalog.parse_ident($1)", &[value.into()])
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "target table name is empty".to_owned())?;
    if parts.is_empty() || parts.len() > 2 {
        return Err("target table must be an optionally schema-qualified name".into());
    }
    let quoted = parts
        .iter()
        .map(|part| quote_ident(part))
        .collect::<Vec<_>>()
        .join(".");
    Ok((quoted, parts))
}

fn create_target(schema: &TableSchema, target_table: &str) -> Result<(), String> {
    let (target_name, parts) = quote_qualified_relation(target_table)?;
    if parts.len() == 2 {
        Spi::run(&format!(
            "CREATE SCHEMA IF NOT EXISTS {}",
            quote_ident(&parts[0])
        ))
        .map_err(|error| error.to_string())?;
    }

    let mut definitions = Vec::new();
    for column in &schema.columns {
        if !column.identity.is_empty() {
            return Err(
                "automatic target creation does not support identity columns; create target first"
                    .into(),
            );
        }
        let mut definition = format!("{} {}", quote_ident(&column.name), column.type_name);
        if let Some(collation) = &column.collation_name {
            definition.push_str(" COLLATE ");
            definition.push_str(collation);
        }
        if let Some(expression) = &column.generated_expr {
            definition.push_str(" GENERATED ALWAYS AS (");
            definition.push_str(expression);
            definition.push_str(") STORED");
        } else if let Some(expression) = &column.default_expr {
            definition.push_str(" DEFAULT ");
            definition.push_str(expression);
        }
        if column.not_null {
            definition.push_str(" NOT NULL");
        }
        definitions.push(definition);
    }
    definitions.extend(schema.constraints.iter().cloned());
    Spi::run(&format!(
        "CREATE TABLE {target_name} ({})",
        definitions.join(", ")
    ))
    .map_err(|error| error.to_string())?;

    // Recreate dropped attribute numbers as well: heap pages retain their physical layout.
    for column in schema.columns.iter().filter(|column| column.dropped) {
        Spi::run(&format!(
            "ALTER TABLE {target_name} DROP COLUMN {}",
            quote_ident(&column.name)
        ))
        .map_err(|error| error.to_string())?;
    }
    for index in &schema.indexes {
        Spi::run(&format!(
            "CREATE {}INDEX ON {target_name} USING {}",
            if index.unique { "UNIQUE " } else { "" },
            index.definition
        ))
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn index_shape(definition: &str) -> &str {
    definition
        .find(" USING ")
        .map_or(definition, |position| &definition[position..])
}

fn same_index_definition(left: &str, right: &str) -> bool {
    left.starts_with("CREATE UNIQUE INDEX") == right.starts_with("CREATE UNIQUE INDEX")
        && index_shape(left) == index_shape(right)
}

#[allow(clippy::cast_possible_wrap)] // PostgreSQL constants are ASCII on either c_char signedness.
fn reset_storage(rel: &Rel) {
    unsafe {
        pg_sys::RelationSetNewRelfilenumber(
            rel.as_ptr(),
            pg_sys::RELPERSISTENCE_PERMANENT as std::os::raw::c_char,
        );
    }
}

fn prepare_state(
    manifest: Manifest,
    target_table: &str,
    target_main: Rel,
    target_toast: Option<Rel>,
    mut target_indexes: Vec<(String, Rel)>,
    target_toast_index: Option<Rel>,
) -> Result<ImportState, String> {
    if manifest.target_table != target_table {
        return Err("source manifest names a different target table".into());
    }
    if manifest.checksums != ffi::data_checksums_enabled() {
        return Err("manifest checksum setting differs from target".into());
    }
    let target_attributes = attributes(&target_main);
    if manifest.attributes != target_attributes {
        return Err(format!(
            "source and target table attributes differ: source={:?}, target={:?}",
            manifest.attributes, target_attributes
        ));
    }

    let source_has_toast = manifest
        .relations
        .iter()
        .any(|relation| relation.kind == RelationKind::Toast);
    if source_has_toast != target_toast.is_some()
        || source_has_toast != target_toast_index.is_some()
    {
        return Err("source and target TOAST layout differs".into());
    }

    let source_index_columns = &manifest.index_definitions;
    if source_index_columns.len() != target_indexes.len() {
        return Err("source and target index counts differ".into());
    }

    let mut targets = Vec::with_capacity(manifest.relations.len());
    targets.push(target_main);
    if source_has_toast {
        targets.push(target_toast.ok_or_else(|| "missing target TOAST relation".to_owned())?);
        targets.push(target_toast_index.ok_or_else(|| "missing target TOAST index".to_owned())?);
    }
    for definition in source_index_columns {
        let position = target_indexes
            .iter()
            .position(|(candidate, _)| same_index_definition(candidate, definition))
            .ok_or_else(|| format!("target lacks source btree index ({definition})"))?;
        let (_, index) = target_indexes.remove(position);
        targets.push(index);
    }
    if targets.len() != manifest.relations.len() {
        return Err("manifest relation count does not match target".into());
    }

    for target in &targets {
        reset_storage(target);
    }
    for (relation, target) in manifest.relations.iter().zip(&targets) {
        if relation.fork != u8::try_from(MAIN_FORK).expect("main fork fits in u8") {
            return Err("only the main fork is supported".into());
        }
        if target.blocks(MAIN_FORK) != 0 {
            return Err(format!("target relation {} is not empty", target.name()));
        }
    }
    let next_blocks = vec![0; targets.len()];
    Ok(ImportState {
        manifest,
        targets,
        next_blocks,
        done: false,
    })
}

/// # Safety
/// Runs inside PostgreSQL. All target relations remain exclusively locked until commit.
#[allow(clippy::too_many_lines)] // One streaming transaction; no hidden background work.
pub unsafe fn import(
    source_conninfo: &str,
    source_table: &str,
    target_table: &str,
    generate_wal: bool,
) -> Result<(), String> {
    let mut source = SourceClient::connect(source_conninfo)?;
    source.prepare_direct(source_table)?;
    let target_oid = if let Ok(oid) = ffi::resolve_relation(target_table) {
        oid
    } else {
        let schema = source.schema(source_table)?;
        create_target(&schema, target_table)?;
        ffi::resolve_relation(target_table)?
    };
    let target_main = Rel::open(target_oid, pg_sys::AccessExclusiveLock.cast_signed());
    if !target_main.is_plain_logged_table() {
        return Err("target must be a plain logged table".into());
    }
    let target_toast_oid = target_main.toast_oid();
    let target_toast = if target_toast_oid == pg_sys::Oid::INVALID {
        None
    } else {
        Some(Rel::open(
            target_toast_oid,
            pg_sys::AccessExclusiveLock.cast_signed(),
        ))
    };
    let target_toast_index = if let Some(ref toast) = target_toast {
        let oid = toast.toast_index_oid();
        if oid == pg_sys::Oid::INVALID {
            return Err("target TOAST relation has no valid index".into());
        }
        let index = Rel::open(oid, pg_sys::AccessExclusiveLock.cast_signed());
        if !index.is_btree() {
            return Err("target TOAST index is not a supported btree".into());
        }
        Some(index)
    } else {
        None
    };

    let mut target_indexes = Vec::new();
    for oid in target_main.index_oids() {
        let index = Rel::open(oid, pg_sys::AccessExclusiveLock.cast_signed());
        if !index.is_btree() {
            return Err(format!(
                "target index {} is not a supported btree",
                index.name()
            ));
        }
        target_indexes.push((ffi::index_definition(oid), index));
    }

    let target_checksums = ffi::data_checksums_enabled();
    let command = format!(
        "COPY (SELECT pg_attach_export_stream({}, {}, {}::oid, {})) TO STDOUT (FORMAT binary)",
        quote_literal(source_table),
        quote_literal(target_table),
        target_toast_oid.to_u32(),
        if target_checksums { "true" } else { "false" },
    );
    let mut state: Option<ImportState> = None;
    let mut target_main = Some(target_main);
    let mut target_toast = target_toast;
    let mut target_toast_index = target_toast_index;

    source.copy_out(&command, |row| {
        if state.as_ref().is_some_and(|state| state.done) {
            return Err("received a row after the stream terminator".into());
        }
        if state.is_none() {
            let reader = wire::StreamReader::new(Cursor::new(row))
                .map_err(|error| format!("invalid pg_attach header: {error}"))?;
            let manifest = reader.manifest;
            let main = target_main
                .take()
                .ok_or_else(|| "target table was already consumed".to_owned())?;
            let next = prepare_state(
                manifest,
                target_table,
                main,
                target_toast.take(),
                std::mem::take(&mut target_indexes),
                target_toast_index.take(),
            )?;
            state = Some(next);
            return Ok(());
        }

        let import = state.as_mut().unwrap();
        let packet = wire::decode_page(row).map_err(|error| error.to_string())?;
        let Some(mut packet) = packet else {
            for (index, relation) in import.manifest.relations.iter().enumerate() {
                if import.next_blocks[index] != relation.blocks {
                    return Err(format!(
                        "stream ended early for {}: block {} of {}",
                        relation.name, import.next_blocks[index], relation.blocks
                    ));
                }
            }
            for target in &import.targets {
                if generate_wal {
                    target.log_relation(MAIN_FORK);
                } else {
                    target.sync_relation(MAIN_FORK);
                }
            }
            import.done = true;
            return Ok(());
        };

        let relation_index = usize::from(packet.relation);
        if relation_index >= import.targets.len() {
            return Err(format!("invalid relation index {}", packet.relation));
        }
        if packet.fork != u8::try_from(MAIN_FORK).expect("main fork fits in u8") {
            return Err("only main-fork packets are supported".into());
        }
        if packet.block != import.next_blocks[relation_index] {
            return Err(format!(
                "out-of-order block {} for relation {}",
                packet.block, relation_index
            ));
        }
        if packet.block >= import.manifest.relations[relation_index].blocks {
            return Err("page exceeds manifest relation length".into());
        }
        let target = &import.targets[relation_index];
        // Debug-only corruption hook for the BDD suite: flips one byte of a
        // chosen heap block to exercise page verification end to end.
        if relation_index == 0 {
            let block = crate::DEBUG_CORRUPT_PAGE.get();
            let offset = crate::DEBUG_CORRUPT_OFFSET.get();
            if block >= 0
                && offset >= 0
                && packet.block == u32::try_from(block).expect("nonnegative block fits u32")
                && usize::try_from(offset).is_ok_and(|offset| offset < packet.bytes.len())
            {
                packet.bytes[usize::try_from(offset).expect("nonnegative offset fits usize")] ^=
                    0xFF;
            }
        }
        if !Rel::verify_page(packet.block, &mut packet.bytes, import.manifest.checksums) {
            return Err(format!(
                "page verification failed for block {} of relation {}",
                packet.block, relation_index
            ));
        }
        target.write_page(MAIN_FORK, packet.block, &packet.bytes);
        import.next_blocks[relation_index] += 1;
        Ok(())
    })?;

    let state = state.ok_or_else(|| "source returned no pg_attach header".to_owned())?;
    if !state.done {
        return Err("source COPY ended without pg_attach terminator".into());
    }
    source.commit()?;
    Ok(())
}
