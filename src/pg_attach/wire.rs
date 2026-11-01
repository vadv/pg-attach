use std::fmt;
use std::io::{self, Read, Write};

pub const MAGIC: [u8; 4] = *b"PGAT";
pub const HEADER_LEN: usize = 10;
pub const PAGE_HEADER_LEN: usize = 12;
const SCHEMA_MAGIC: [u8; 4] = *b"PGAS";
const SCHEMA_HEADER_LEN: usize = 6;
const MAX_STRING: usize = 1024 * 1024;
const MAX_RELATIONS: usize = 1024;
const MAX_ATTRIBUTES: usize = 1600;

#[derive(Debug)]
pub struct WireError(pub String);

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<io::Error> for WireError {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<WireError> for String {
    fn from(error: WireError) -> Self {
        error.0
    }
}

pub type Result<T> = std::result::Result<T, WireError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RelationKind {
    Main = 0,
    Toast = 1,
    Index = 2,
}

impl RelationKind {
    fn decode(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Main),
            1 => Ok(Self::Toast),
            2 => Ok(Self::Index),
            _ => Err(WireError(format!("unknown relation kind {value}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relation {
    pub oid: u32,
    pub name: String,
    pub kind: RelationKind,
    pub fork: u8,
    pub blocks: u32,
    pub toast_index_oid: u32,
}

#[allow(clippy::struct_excessive_bools)] // These are independent PostgreSQL catalog flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub type_name: String,
    pub collation_name: Option<String>,
    pub collation_version: String,
    pub collation_actual_version: String,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexMetadata {
    pub shape: String,
    pub btree_version: u32,
    pub all_equal_image: bool,
    pub expected_all_equal_image: bool,
}

pub fn validate_attributes(attributes: &[Attribute]) -> std::result::Result<(), String> {
    const SAFE_TYPES: &[&str] = &[
        "bool",
        "bytea",
        "char",
        "name",
        "int8",
        "int2",
        "int4",
        "text",
        "json",
        "xml",
        "point",
        "path",
        "polygon",
        "line",
        "circle",
        "box",
        "float4",
        "float8",
        "bpchar",
        "varchar",
        "date",
        "time",
        "timestamp",
        "timestamptz",
        "interval",
        "numeric",
        "uuid",
        "jsonb",
        "money",
        "inet",
        "cidr",
        "macaddr",
        "macaddr8",
        "bit",
        "varbit",
        "pg_lsn",
        "tsvector",
        "tsquery",
        "jsonpath",
    ];
    for attribute in attributes.iter().filter(|attribute| !attribute.dropped) {
        if attribute.has_missing {
            return Err(format!(
                "column {} uses fast defaults/missing values, which are not supported",
                attribute.name
            ));
        }
        if attribute.generated == b'v' {
            return Err(format!(
                "column {} is a virtual generated column, which is not supported",
                attribute.name
            ));
        }
        if attribute.generated != 0 && attribute.generated != b's' {
            return Err(format!(
                "column {} has an unknown generated kind",
                attribute.name
            ));
        }
        let base = attribute
            .type_name
            .strip_prefix("pg_catalog._")
            .or_else(|| attribute.type_name.strip_prefix("pg_catalog."));
        if !base.is_some_and(|base| SAFE_TYPES.contains(&base)) {
            return Err(format!(
                "column {} has unsafe type {}; only built-in types without catalog OIDs in their values are supported",
                attribute.name, attribute.type_name
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaColumn {
    pub name: String,
    pub type_name: String,
    pub collation_name: Option<String>,
    pub default_expr: Option<String>,
    pub not_null: bool,
    pub identity: String,
    pub generated_expr: Option<String>,
    pub storage: u8,
    pub dropped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaIndex {
    /// The part after `USING`, deliberately independent of source table/index names.
    pub definition: String,
    pub unique: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableSchema {
    pub columns: Vec<SchemaColumn>,
    pub constraints: Vec<String>,
    pub indexes: Vec<SchemaIndex>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub source_table: String,
    pub target_table: String,
    pub source_checksums: bool,
    pub target_checksums: bool,
    pub relations: Vec<Relation>,
    pub attributes: Vec<Attribute>,
    pub indexes: Vec<IndexMetadata>,
}

impl Manifest {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        put_string(&mut out, &self.source_table)?;
        put_string(&mut out, &self.target_table)?;
        out.push(u8::from(self.source_checksums));
        out.push(u8::from(self.target_checksums));
        put_len(&mut out, self.relations.len(), MAX_RELATIONS, "relations")?;
        for relation in &self.relations {
            put_u32(&mut out, relation.oid)?;
            put_string(&mut out, &relation.name)?;
            out.push(relation.kind as u8);
            out.push(relation.fork);
            put_u32(&mut out, relation.blocks)?;
            put_u32(&mut out, relation.toast_index_oid)?;
        }
        put_len(
            &mut out,
            self.attributes.len(),
            MAX_ATTRIBUTES,
            "attributes",
        )?;
        for attribute in &self.attributes {
            put_string(&mut out, &attribute.name)?;
            put_string(&mut out, &attribute.type_name)?;
            put_optional_string(&mut out, attribute.collation_name.as_deref())?;
            put_string(&mut out, &attribute.collation_version)?;
            put_string(&mut out, &attribute.collation_actual_version)?;
            out.extend_from_slice(&attribute.typmod.to_be_bytes());
            out.extend_from_slice(&attribute.len.to_be_bytes());
            out.push(attribute.align);
            out.push(u8::from(attribute.by_val));
            out.push(attribute.storage);
            out.push(u8::from(attribute.has_missing));
            out.push(attribute.generated);
            out.push(u8::from(attribute.not_null));
            out.push(u8::from(attribute.dropped));
        }
        put_len(&mut out, self.indexes.len(), MAX_RELATIONS, "indexes")?;
        for index in &self.indexes {
            put_string(&mut out, &index.shape)?;
            put_u32(&mut out, index.btree_version)?;
            out.push(u8::from(index.all_equal_image));
            out.push(u8::from(index.expected_all_equal_image));
        }
        Ok(out)
    }

    pub fn decode(input: &[u8]) -> Result<Self> {
        let mut decoder = Decoder { input, offset: 0 };
        let source_table = decoder.string()?;
        let target_table = decoder.string()?;
        let source_checksums = decoder.boolean()?;
        let target_checksums = decoder.boolean()?;
        let relation_count = decoder.len(MAX_RELATIONS, "relations")?;
        let mut relations = Vec::with_capacity(relation_count);
        for _ in 0..relation_count {
            relations.push(Relation {
                oid: decoder.u32()?,
                name: decoder.string()?,
                kind: RelationKind::decode(decoder.u8()?)?,
                fork: decoder.u8()?,
                blocks: decoder.u32()?,
                toast_index_oid: decoder.u32()?,
            });
        }
        let attribute_count = decoder.len(MAX_ATTRIBUTES, "attributes")?;
        let mut attributes = Vec::with_capacity(attribute_count);
        for _ in 0..attribute_count {
            attributes.push(Attribute {
                name: decoder.string()?,
                type_name: decoder.string()?,
                collation_name: decoder.optional_string()?,
                collation_version: decoder.string()?,
                collation_actual_version: decoder.string()?,
                typmod: decoder.i32()?,
                len: decoder.i16()?,
                align: decoder.u8()?,
                by_val: decoder.boolean()?,
                storage: decoder.u8()?,
                has_missing: decoder.boolean()?,
                generated: decoder.u8()?,
                not_null: decoder.boolean()?,
                dropped: decoder.boolean()?,
            });
        }
        let index_count = decoder.len(MAX_RELATIONS, "indexes")?;
        let mut indexes = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            indexes.push(IndexMetadata {
                shape: decoder.string()?,
                btree_version: decoder.u32()?,
                all_equal_image: decoder.boolean()?,
                expected_all_equal_image: decoder.boolean()?,
            });
        }
        if decoder.offset != input.len() {
            return Err(WireError("trailing bytes in manifest".into()));
        }
        Ok(Self {
            source_table,
            target_table,
            source_checksums,
            target_checksums,
            relations,
            attributes,
            indexes,
        })
    }
}

pub fn encode_schema(schema: &TableSchema) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&SCHEMA_MAGIC);
    out.extend_from_slice(&0u16.to_be_bytes());
    put_len(
        &mut out,
        schema.columns.len(),
        MAX_ATTRIBUTES,
        "schema columns",
    )?;
    for column in &schema.columns {
        put_string(&mut out, &column.name)?;
        put_string(&mut out, &column.type_name)?;
        put_optional_string(&mut out, column.collation_name.as_deref())?;
        put_optional_string(&mut out, column.default_expr.as_deref())?;
        out.push(u8::from(column.not_null));
        put_string(&mut out, &column.identity)?;
        put_optional_string(&mut out, column.generated_expr.as_deref())?;
        out.push(column.storage);
        out.push(u8::from(column.dropped));
    }
    put_len(
        &mut out,
        schema.constraints.len(),
        MAX_RELATIONS,
        "constraints",
    )?;
    for constraint in &schema.constraints {
        put_string(&mut out, constraint)?;
    }
    put_len(
        &mut out,
        schema.indexes.len(),
        MAX_RELATIONS,
        "schema indexes",
    )?;
    for index in &schema.indexes {
        put_string(&mut out, &index.definition)?;
        out.push(u8::from(index.unique));
    }
    Ok(out)
}

pub fn decode_schema(input: &[u8]) -> Result<TableSchema> {
    if input.len() < SCHEMA_HEADER_LEN || input[..4] != SCHEMA_MAGIC {
        return Err(WireError("bad pg_attach schema magic".into()));
    }
    if input[4] != 0 || input[5] != 0 {
        return Err(WireError("unsupported pg_attach schema header".into()));
    }
    let mut decoder = Decoder {
        input: &input[SCHEMA_HEADER_LEN..],
        offset: 0,
    };
    let column_count = decoder.len(MAX_ATTRIBUTES, "schema columns")?;
    let mut columns = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        columns.push(SchemaColumn {
            name: decoder.string()?,
            type_name: decoder.string()?,
            collation_name: decoder.optional_string()?,
            default_expr: decoder.optional_string()?,
            not_null: decoder.boolean()?,
            identity: decoder.string()?,
            generated_expr: decoder.optional_string()?,
            storage: decoder.u8()?,
            dropped: decoder.boolean()?,
        });
    }
    let constraint_count = decoder.len(MAX_RELATIONS, "constraints")?;
    let mut constraints = Vec::with_capacity(constraint_count);
    for _ in 0..constraint_count {
        constraints.push(decoder.string()?);
    }
    let index_count = decoder.len(MAX_RELATIONS, "schema indexes")?;
    let mut indexes = Vec::with_capacity(index_count);
    for _ in 0..index_count {
        indexes.push(SchemaIndex {
            definition: decoder.string()?,
            unique: decoder.boolean()?,
        });
    }
    if decoder.offset != decoder.input.len() {
        return Err(WireError("trailing bytes in schema descriptor".into()));
    }
    Ok(TableSchema {
        columns,
        constraints,
        indexes,
    })
}

#[cfg(test)]
pub struct StreamWriter<W> {
    inner: W,
}

#[cfg(test)]
impl<W: Write> StreamWriter<W> {
    pub fn new(mut inner: W, manifest: &Manifest) -> Result<Self> {
        inner.write_all(&encode_header(manifest)?)?;
        Ok(Self { inner })
    }

    pub fn page(&mut self, relation: u16, fork: u8, block: u32, page: &[u8]) -> Result<()> {
        self.inner
            .write_all(&encode_page(relation, fork, block, page)?)?;
        Ok(())
    }

    pub fn done(mut self) -> Result<W> {
        self.inner.write_all(&[0, 0, 0, 0])?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

pub fn encode_header(manifest: &Manifest) -> Result<Vec<u8>> {
    let manifest = manifest.encode()?;
    let mut output = Vec::with_capacity(HEADER_LEN + manifest.len());
    output.extend_from_slice(&MAGIC);
    output.extend_from_slice(&0u16.to_be_bytes());
    put_u32(
        &mut output,
        u32::try_from(manifest.len()).map_err(|_| WireError("manifest too large".into()))?,
    )?;
    output.extend_from_slice(&manifest);
    Ok(output)
}

pub fn encode_page(relation: u16, fork: u8, block: u32, page: &[u8]) -> Result<Vec<u8>> {
    if page.len() != 8192 {
        return Err(WireError(format!("invalid page size {}", page.len())));
    }
    let mut output = Vec::with_capacity(PAGE_HEADER_LEN + page.len());
    put_u16(&mut output, relation)?;
    output.extend_from_slice(&[fork, 0]);
    put_u32(&mut output, block)?;
    put_u32(
        &mut output,
        u32::try_from(page.len()).expect("page length fits u32"),
    )?;
    output.extend_from_slice(page);
    Ok(output)
}

pub struct StreamReader<R> {
    #[allow(dead_code)] // Production consumes only the header; tests also read pages.
    inner: R,
    pub manifest: Manifest,
}

impl<R: Read> StreamReader<R> {
    pub fn new(mut inner: R) -> Result<Self> {
        let mut header = [0u8; HEADER_LEN];
        inner.read_exact(&mut header)?;
        if header[..4] != MAGIC {
            return Err(WireError("bad pg_attach magic".into()));
        }
        if header[4] != 0 || header[5] != 0 {
            return Err(WireError("unsupported pg_attach header flags".into()));
        }
        let length = usize::try_from(u32::from_be_bytes(header[6..10].try_into().unwrap()))
            .expect("u32 length fits usize");
        if length == 0 || length > MAX_STRING * 16 {
            return Err(WireError(format!("invalid manifest length {length}")));
        }
        let mut manifest = vec![0; length];
        inner.read_exact(&mut manifest)?;
        let manifest = Manifest::decode(&manifest)?;
        Ok(Self { inner, manifest })
    }

    #[cfg(test)]
    pub fn next(&mut self) -> Result<Option<PagePacket>> {
        let mut first = [0u8; 4];
        match self.inner.read_exact(&mut first) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(WireError("truncated pg_attach stream".into()));
            }
            Err(error) => return Err(error.into()),
        }
        let mut header = [0u8; PAGE_HEADER_LEN];
        header[..4].copy_from_slice(&first);
        if first == [0, 0, 0, 0] {
            let mut fifth = [0u8; 1];
            match self.inner.read_exact(&mut fifth) {
                Ok(()) => header[4] = fifth[0],
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            }
            self.inner.read_exact(&mut header[5..])?;
        } else {
            self.inner.read_exact(&mut header[4..])?;
        }
        if header[3] != 0 {
            return Err(WireError("unsupported page packet flags".into()));
        }
        let length = usize::try_from(u32::from_be_bytes(header[8..12].try_into().unwrap()))
            .expect("u32 length fits usize");
        if length != 8192 {
            return Err(WireError(format!("invalid page payload length {length}")));
        }
        let mut bytes = vec![0; length];
        self.inner.read_exact(&mut bytes)?;
        Ok(Some(PagePacket {
            relation: u16::from_be_bytes([header[0], header[1]]),
            fork: header[2],
            block: u32::from_be_bytes(header[4..8].try_into().unwrap()),
            bytes,
        }))
    }
}

pub fn decode_page(input: &[u8]) -> Result<Option<PagePacket>> {
    if input == [0, 0, 0, 0] {
        return Ok(None);
    }
    if input.len() != PAGE_HEADER_LEN + 8192 {
        return Err(WireError(format!(
            "invalid page row length {}",
            input.len()
        )));
    }
    if input[3] != 0 {
        return Err(WireError("unsupported page packet flags".into()));
    }
    let length = usize::try_from(u32::from_be_bytes(input[8..12].try_into().unwrap()))
        .expect("u32 length fits usize");
    if length != 8192 {
        return Err(WireError(format!("invalid page payload length {length}")));
    }
    Ok(Some(PagePacket {
        relation: u16::from_be_bytes([input[0], input[1]]),
        fork: input[2],
        block: u32::from_be_bytes(input[4..8].try_into().unwrap()),
        bytes: input[PAGE_HEADER_LEN..].to_vec(),
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagePacket {
    pub relation: u16,
    pub fork: u8,
    pub block: u32,
    pub bytes: Vec<u8>,
}

fn put_u16(output: &mut impl Write, value: u16) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn put_u32(output: &mut impl Write, value: u32) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn put_len(output: &mut Vec<u8>, len: usize, max: usize, what: &str) -> Result<()> {
    if len > max {
        return Err(WireError(format!("too many {what}: {len}")));
    }
    put_u32(
        output,
        u32::try_from(len).map_err(|_| WireError(format!("too many {what}: {len}")))?,
    )?;
    Ok(())
}

fn put_string(output: &mut Vec<u8>, value: &str) -> Result<()> {
    if value.len() > MAX_STRING {
        return Err(WireError("string exceeds wire limit".into()));
    }
    put_u32(
        output,
        u32::try_from(value.len()).map_err(|_| WireError("string exceeds wire limit".into()))?,
    )?;
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_optional_string(output: &mut Vec<u8>, value: Option<&str>) -> Result<()> {
    output.push(u8::from(value.is_some()));
    if let Some(value) = value {
        put_string(output, value)?;
    }
    Ok(())
}

struct Decoder<'a> {
    input: &'a [u8],
    offset: usize,
}

impl Decoder<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| WireError("length overflow".into()))?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or_else(|| WireError("truncated manifest".into()))?;
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn boolean(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(WireError(format!("invalid boolean {value}"))),
        }
    }

    fn len(&mut self, max: usize, what: &str) -> Result<usize> {
        let len = usize::try_from(self.u32()?).expect("u32 length fits usize");
        if len > max {
            Err(WireError(format!("too many {what}: {len}")))
        } else {
            Ok(len)
        }
    }

    fn string(&mut self) -> Result<String> {
        let length = usize::try_from(self.u32()?).expect("u32 length fits usize");
        if length > MAX_STRING {
            return Err(WireError("string exceeds wire limit".into()));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| WireError("manifest string is not UTF-8".into()))
    }

    fn optional_string(&mut self) -> Result<Option<String>> {
        if self.boolean()? {
            Ok(Some(self.string()?))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribute() -> Attribute {
        Attribute {
            name: "id".into(),
            type_name: "pg_catalog.int4".into(),
            collation_name: None,
            collation_version: String::new(),
            collation_actual_version: String::new(),
            typmod: -1,
            len: 4,
            align: b'i',
            by_val: true,
            storage: b'p',
            has_missing: false,
            generated: 0,
            not_null: true,
            dropped: false,
        }
    }

    fn manifest() -> Manifest {
        Manifest {
            source_table: "public.events".into(),
            target_table: "public.events_copy".into(),
            source_checksums: true,
            target_checksums: true,
            relations: vec![Relation {
                oid: 42,
                name: "events".into(),
                kind: RelationKind::Main,
                fork: 0,
                blocks: 1,
                toast_index_oid: 0,
            }],
            attributes: vec![attribute()],
            indexes: vec![IndexMetadata {
                shape: "1|1|1|int4_ops".into(),
                btree_version: 4,
                all_equal_image: true,
                expected_all_equal_image: true,
            }],
        }
    }

    #[test]
    fn rejects_missing_values_virtual_columns_and_oid_bearing_types() {
        let missing = Attribute {
            has_missing: true,
            ..attribute()
        };
        assert!(validate_attributes(&[missing])
            .unwrap_err()
            .contains("missing values"));
        let virtual_column = Attribute {
            generated: b'v',
            ..attribute()
        };
        assert!(validate_attributes(&[virtual_column])
            .unwrap_err()
            .contains("virtual generated"));
        for type_name in [
            "public.mood",
            "pg_catalog._mood",
            "pg_catalog.regclass",
            "pg_catalog.oid",
            "pg_catalog.pg_class",
        ] {
            let unsafe_type = Attribute {
                type_name: type_name.into(),
                ..attribute()
            };
            assert!(validate_attributes(&[unsafe_type])
                .unwrap_err()
                .contains("unsafe type"));
        }
        let safe_array = Attribute {
            type_name: "pg_catalog._int4".into(),
            ..attribute()
        };
        assert!(validate_attributes(&[safe_array]).is_ok());
    }

    #[test]
    fn schema_round_trip_preserves_target_independent_definitions() {
        let value = TableSchema {
            columns: vec![SchemaColumn {
                name: "payload".into(),
                type_name: "text".into(),
                collation_name: Some("pg_catalog.\"C\"".into()),
                default_expr: Some("'default'::text".into()),
                not_null: true,
                identity: String::new(),
                generated_expr: None,
                storage: b'e',
                dropped: false,
            }],
            constraints: vec!["CHECK (length(payload) > 0)".into()],
            indexes: vec![SchemaIndex {
                definition: "btree (payload)".into(),
                unique: true,
            }],
        };
        let encoded = encode_schema(&value).unwrap();
        assert_eq!(decode_schema(&encoded).unwrap(), value);
        assert!(!encoded
            .windows(b"source_table".len())
            .any(|w| w == b"source_table"));
    }

    #[test]
    fn manifest_round_trip() {
        let value = manifest();
        assert_eq!(Manifest::decode(&value.encode().unwrap()).unwrap(), value);
    }

    #[test]
    fn page_stream_round_trip() {
        let value = manifest();
        let mut bytes = Vec::new();
        let mut writer = StreamWriter::new(&mut bytes, &value).unwrap();
        writer.page(0, 0, 7, &[5; 8192]).unwrap();
        writer.done().unwrap();
        let mut reader = StreamReader::new(bytes.as_slice()).unwrap();
        let packet = reader.next().unwrap().unwrap();
        assert_eq!(packet.block, 7);
        assert_eq!(packet.bytes, vec![5; 8192]);
        assert!(reader.next().unwrap().is_none());
    }

    #[test]
    fn headers_use_unversioned_layout() {
        let header = encode_header(&manifest()).unwrap();
        assert_eq!(HEADER_LEN, 10);
        assert_eq!(&header[..6], b"PGAT\0\0");
        let manifest_length =
            usize::try_from(u32::from_be_bytes(header[6..10].try_into().unwrap())).unwrap();
        assert_eq!(manifest_length, header.len() - HEADER_LEN);

        let schema = encode_schema(&TableSchema {
            columns: Vec::new(),
            constraints: Vec::new(),
            indexes: Vec::new(),
        })
        .unwrap();
        assert_eq!(SCHEMA_HEADER_LEN, 6);
        assert_eq!(&schema[..6], b"PGAS\0\0");
    }

    #[test]
    fn rejects_bad_flags_and_truncated_headers() {
        let mut header = encode_header(&manifest()).unwrap();
        header[4] = 1;
        assert!(StreamReader::new(header.as_slice()).is_err());
        assert!(StreamReader::new(&header[..HEADER_LEN - 1]).is_err());

        let mut schema = encode_schema(&TableSchema {
            columns: Vec::new(),
            constraints: Vec::new(),
            indexes: Vec::new(),
        })
        .unwrap();
        schema[4] = 1;
        assert!(decode_schema(&schema).is_err());
        assert!(decode_schema(&schema[..SCHEMA_HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = vec![0; HEADER_LEN];
        bytes[..4].copy_from_slice(b"NOPE");
        assert!(StreamReader::new(bytes.as_slice()).is_err());
    }
}
