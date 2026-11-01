use super::ffi;
use super::wire::{self, TableSchema};
use pgrx::{pg_sys, spi::Spi};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

#[allow(non_camel_case_types)]
enum PGconn {}
#[allow(non_camel_case_types)]
enum PGresult {}

const CONNECTION_OK: c_int = 0;
const PGRES_COMMAND_OK: c_int = 1;
const PGRES_TUPLES_OK: c_int = 2;
const PGRES_COPY_OUT: c_int = 3;

#[link(name = "pq")]
unsafe extern "C" {
    fn PQconnectdb(conninfo: *const c_char) -> *mut PGconn;
    fn PQstatus(conn: *const PGconn) -> c_int;
    fn PQerrorMessage(conn: *const PGconn) -> *const c_char;
    fn PQfinish(conn: *mut PGconn);
    fn PQexec(conn: *mut PGconn, query: *const c_char) -> *mut PGresult;
    fn PQexecParams(
        conn: *mut PGconn,
        command: *const c_char,
        n_params: c_int,
        param_types: *const c_void,
        param_values: *const *const c_char,
        param_lengths: *const c_int,
        param_formats: *const c_int,
        result_format: c_int,
    ) -> *mut PGresult;
    fn PQresultStatus(result: *const PGresult) -> c_int;
    fn PQntuples(result: *const PGresult) -> c_int;
    fn PQnfields(result: *const PGresult) -> c_int;
    fn PQgetvalue(result: *const PGresult, row: c_int, column: c_int) -> *mut c_char;
    fn PQgetisnull(result: *const PGresult, row: c_int, column: c_int) -> c_int;
    fn PQresultErrorMessage(result: *const PGresult) -> *const c_char;
    fn PQclear(result: *mut PGresult);
    fn PQgetCopyData(conn: *mut PGconn, buffer: *mut *mut c_char, async_: c_int) -> c_int;
    fn PQfreemem(pointer: *mut c_void);
    fn PQgetResult(conn: *mut PGconn) -> *mut PGresult;
}

pub struct SourceClient {
    conn: *mut PGconn,
    conninfo: String,
}

#[derive(Clone, Debug)]
struct ServerInfo {
    server_version_num: i32,
    block_size: i32,
    max_align: i32,
    endianness: String,
    server_encoding: String,
    data_checksums: bool,
    extension_version: String,
    max_toast_chunk_size: i32,
    max_identifier_length: i32,
    checksum_algorithm_version: i32,
}

impl ServerInfo {
    fn local() -> Result<Self, String> {
        let server_encoding = Spi::connect(|client| {
            client
                .select("SELECT current_setting('server_encoding')", Some(1), &[])?
                .first()
                .get_one::<String>()
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "target server encoding is NULL".to_owned())?;
        let extension_version =
            Spi::connect(|client| {
                client.select(
                "SELECT extversion FROM pg_catalog.pg_extension WHERE extname = 'pg_attach'",
                Some(1),
                &[],
            )?.first().get_one::<String>()
            })
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "target extension version is missing".to_owned())?;
        Ok(Self {
            server_version_num: i32::try_from(pg_sys::PG_VERSION_NUM)
                .expect("server version fits i32"),
            block_size: i32::try_from(pg_sys::BLCKSZ).expect("block size fits i32"),
            max_align: i32::try_from(pg_sys::MAXIMUM_ALIGNOF).expect("alignment fits i32"),
            endianness: if cfg!(target_endian = "little") {
                "little"
            } else {
                "big"
            }
            .to_owned(),
            server_encoding,
            data_checksums: ffi::data_checksums_enabled(),
            extension_version,
            max_toast_chunk_size: ffi::max_toast_chunk_size(),
            max_identifier_length: ffi::max_identifier_length(),
            checksum_algorithm_version: ffi::checksum_algorithm_version(),
        })
    }

    fn ensure_compatible(&self, target: &Self) -> Result<(), String> {
        let source_major = self.server_version_num / 10000;
        let target_major = target.server_version_num / 10000;
        if !(16..=18).contains(&source_major)
            || !(16..=18).contains(&target_major)
            || source_major > target_major
        {
            return Err(format!(
                "unsupported PostgreSQL major pair {source_major} -> {target_major}; only same-major and upward 16-18 transfers are supported"
            ));
        }
        ensure_equal(
            "extension version",
            &self.extension_version,
            &target.extension_version,
        )?;
        ensure_equal("block size", &self.block_size, &target.block_size)?;
        ensure_equal("max align", &self.max_align, &target.max_align)?;
        ensure_equal("endianness", &self.endianness, &target.endianness)?;
        ensure_equal(
            "server encoding",
            &self.server_encoding,
            &target.server_encoding,
        )?;
        ensure_equal(
            "data checksums",
            &self.data_checksums,
            &target.data_checksums,
        )?;
        ensure_equal(
            "TOAST chunk size",
            &self.max_toast_chunk_size,
            &target.max_toast_chunk_size,
        )?;
        ensure_equal(
            "identifier length",
            &self.max_identifier_length,
            &target.max_identifier_length,
        )?;
        ensure_equal(
            "checksum algorithm",
            &self.checksum_algorithm_version,
            &target.checksum_algorithm_version,
        )
    }

    fn decode(result: *mut PGresult) -> Result<Self, String> {
        let value = |column| -> Result<String, String> {
            if unsafe { PQgetisnull(result, 0, column) } != 0 {
                return Err(format!("source server info column {column} is NULL"));
            }
            let value = unsafe { CStr::from_ptr(PQgetvalue(result, 0, column)) }
                .to_str()
                .map_err(|_| format!("source server info column {column} is not UTF-8"))?;
            Ok(value.to_owned())
        };
        let info = Self {
            server_version_num: parse_i32(&value(0)?, "server version")?,
            block_size: parse_i32(&value(1)?, "block size")?,
            max_align: parse_i32(&value(2)?, "max align")?,
            endianness: value(3)?,
            server_encoding: value(4)?,
            data_checksums: parse_bool(&value(5)?, "data checksums")?,
            extension_version: value(6)?,
            max_toast_chunk_size: parse_i32(&value(7)?, "TOAST chunk size")?,
            max_identifier_length: parse_i32(&value(8)?, "identifier length")?,
            checksum_algorithm_version: parse_i32(&value(9)?, "checksum algorithm")?,
        };
        parse_bool(&value(10)?, "lz4 support")?;
        Ok(info)
    }
}

fn ensure_equal<T: PartialEq + std::fmt::Display>(
    field: &str,
    source: &T,
    target: &T,
) -> Result<(), String> {
    if source == target {
        Ok(())
    } else {
        Err(format!(
            "source {field} {source} does not match target {target}"
        ))
    }
}

fn parse_i32(value: &str, field: &str) -> Result<i32, String> {
    value
        .parse()
        .map_err(|_| format!("invalid source {field}: {value}"))
}

fn parse_bool(value: &str, field: &str) -> Result<bool, String> {
    match value {
        "t" => Ok(true),
        "f" => Ok(false),
        _ => Err(format!("invalid source {field}: {value}")),
    }
}

fn strip_copy_file_header(row: &[u8]) -> Result<&[u8], String> {
    const HEADER_LEN: usize = 19;
    if row.len() < HEADER_LEN || &row[..11] != b"PGCOPY\n\xff\r\n\0" {
        return Err("invalid binary COPY file header".into());
    }
    Ok(&row[HEADER_LEN..])
}

/// Extract the single bytea field from one libpq binary COPY row.
/// The COPY file header is consumed by libpq; each row still has this envelope.
fn copy_binary_payload(row: &[u8]) -> Result<Option<&[u8]>, String> {
    if row.len() < 2 {
        return Err("truncated binary COPY row".into());
    }
    let fields = i16::from_be_bytes([row[0], row[1]]);
    if fields == -1 {
        return if row.len() == 2 {
            Ok(None) // binary COPY trailer
        } else {
            Err("invalid binary COPY trailer".into())
        };
    }
    if fields != 1 || row.len() < 6 {
        return Err("pg_attach COPY row must contain one field".into());
    }
    let length = i32::from_be_bytes([row[2], row[3], row[4], row[5]]);
    let length = usize::try_from(length).map_err(|_| "negative bytea length")?;
    if row.len() != 6 + length {
        return Err("invalid bytea length in binary COPY row".into());
    }
    Ok(Some(&row[6..]))
}

impl SourceClient {
    pub fn connect(conninfo: &str) -> Result<Self, String> {
        let conninfo_string = conninfo.to_owned();
        let conninfo =
            CString::new(conninfo).map_err(|_| "connection string contains NUL".to_owned())?;
        let conn = unsafe { PQconnectdb(conninfo.as_ptr()) };
        if conn.is_null() {
            return Err("PQconnectdb returned NULL".into());
        }
        let client = Self {
            conn,
            conninfo: conninfo_string,
        };
        if unsafe { PQstatus(conn) } != CONNECTION_OK {
            return Err(client.error());
        }
        Ok(client)
    }

    fn error(&self) -> String {
        unsafe { CStr::from_ptr(PQerrorMessage(self.conn)) }
            .to_string_lossy()
            .trim()
            .to_owned()
    }

    fn execute(&mut self, command: &str) -> Result<(), String> {
        let command = CString::new(command).map_err(|_| "SQL command contains NUL".to_owned())?;
        let result = unsafe { PQexec(self.conn, command.as_ptr()) };
        if result.is_null() {
            return Err(self.error());
        }
        let status = unsafe { PQresultStatus(result) };
        let message = unsafe { CStr::from_ptr(PQresultErrorMessage(result)) }
            .to_string_lossy()
            .trim()
            .to_owned();
        unsafe { PQclear(result) };
        if status != PGRES_COMMAND_OK && status != PGRES_TUPLES_OK {
            return Err(if message.is_empty() {
                self.error()
            } else {
                message
            });
        }
        Ok(())
    }

    fn query_text(&mut self, query: &str, value: &str) -> Result<String, String> {
        let query = CString::new(query).map_err(|_| "SQL query contains NUL".to_owned())?;
        let value = CString::new(value).map_err(|_| "SQL parameter contains NUL".to_owned())?;
        let value_pointer = value.as_ptr();
        let result = unsafe {
            PQexecParams(
                self.conn,
                query.as_ptr(),
                1,
                std::ptr::null(),
                &raw const value_pointer,
                std::ptr::null(),
                std::ptr::null(),
                0,
            )
        };
        if result.is_null() {
            return Err(self.error());
        }
        let status = unsafe { PQresultStatus(result) };
        let message = unsafe { CStr::from_ptr(PQresultErrorMessage(result)) }
            .to_string_lossy()
            .trim()
            .to_owned();
        let text = if status == PGRES_TUPLES_OK && unsafe { PQntuples(result) } == 1 {
            Ok(unsafe { CStr::from_ptr(PQgetvalue(result, 0, 0)) }
                .to_string_lossy()
                .into_owned())
        } else if status == PGRES_TUPLES_OK {
            Err("source relation does not exist".to_owned())
        } else if message.is_empty() {
            Err(self.error())
        } else {
            Err(message)
        };
        unsafe { PQclear(result) };
        text
    }

    fn server_info(&mut self) -> Result<ServerInfo, String> {
        let result = unsafe {
            PQexec(
                self.conn,
                c"SELECT server_version_num, block_size, max_align, endianness, server_encoding, data_checksums, extension_version, max_toast_chunk_size, max_identifier_length, checksum_algorithm_version, lz4_supported FROM pg_attach_server_info()"
                    .as_ptr(),
            )
        };
        if result.is_null() {
            return Err(self.error());
        }
        let info = (|| {
            if unsafe { PQresultStatus(result) } != PGRES_TUPLES_OK {
                let message = unsafe { CStr::from_ptr(PQresultErrorMessage(result)) }
                    .to_string_lossy()
                    .trim()
                    .to_owned();
                return Err(if message.is_empty() {
                    self.error()
                } else {
                    message
                });
            }
            if unsafe { PQntuples(result) } != 1 || unsafe { PQnfields(result) } != 11 {
                return Err("source server info must return one row with eleven columns".to_owned());
            }
            ServerInfo::decode(result)
        })();
        unsafe { PQclear(result) };
        info
    }

    pub fn check_compatibility(&mut self) -> Result<(), String> {
        let source = self
            .server_info()
            .map_err(|error| format!("source compatibility check failed: {error}"))?;
        source.ensure_compatible(&ServerInfo::local()?)
    }

    /// Freeze, lock and validate before checkpointing from a second backend.
    /// A writer between VACUUM and LOCK causes a bounded retry before target changes.
    pub fn prepare_direct(&mut self, source_table: &str) -> Result<(), String> {
        // shortcut: three attempts bound writer starvation; add a policy only for production.
        const MAX_ATTEMPTS: u8 = 3;
        let literal = format!("'{}'", source_table.replace('\'', "''"));
        for attempt in 1..=MAX_ATTEMPTS {
            let vacuum = self.query_text(
                "SELECT format('VACUUM (FREEZE) %I.%I', parts[1], parts[2]) \
                 FROM (SELECT parse_ident($1) AS parts) AS parsed \
                 WHERE array_length(parts, 1) = 2",
                source_table,
            )?;
            self.execute(&vacuum)?;
            self.execute("BEGIN")?;
            let locked = self.execute(&format!("SELECT pg_attach_lock_source({literal})"));
            if let Err(error) = locked {
                let _ = self.execute("ROLLBACK");
                return Err(error);
            }
            match self.query_text("SELECT pg_attach_source_is_frozen($1)", source_table) {
                Ok(value) if value == "t" => {
                    let checkpoint = Self::connect(&self.conninfo)
                        .and_then(|mut client| client.execute("CHECKPOINT"));
                    if let Err(error) = checkpoint {
                        let _ = self.execute("ROLLBACK");
                        return Err(error);
                    }
                    return Ok(());
                }
                Ok(value) if value == "f" => {
                    self.execute("ROLLBACK")?;
                    if attempt == MAX_ATTEMPTS {
                        return Err(
                            "source table did not remain frozen after 3 VACUUM FREEZE attempts"
                                .to_owned(),
                        );
                    }
                }
                Ok(value) => {
                    let _ = self.execute("ROLLBACK");
                    return Err(format!("unexpected frozen validation result: {value}"));
                }
                Err(error) => {
                    let _ = self.execute("ROLLBACK");
                    return Err(error);
                }
            }
        }
        unreachable!("bounded VACUUM FREEZE loop always returns")
    }

    pub fn commit(&mut self) -> Result<(), String> {
        self.execute("COMMIT")
    }

    pub fn schema(&mut self, source_table: &str) -> Result<TableSchema, String> {
        let literal = format!("'{}'", source_table.replace('\'', "''"));
        let command =
            format!("COPY (SELECT pg_attach_export_schema({literal})) TO STDOUT (FORMAT binary)");
        let mut descriptor = None;
        self.copy_out(&command, |row| {
            if descriptor.replace(row.to_vec()).is_some() {
                return Err("source returned more than one schema descriptor".into());
            }
            Ok(())
        })?;
        let descriptor =
            descriptor.ok_or_else(|| "source returned no schema descriptor".to_owned())?;
        wire::decode_schema(&descriptor).map_err(|error| error.to_string())
    }

    pub fn copy_out<F>(&mut self, command: &str, mut sink: F) -> Result<(), String>
    where
        F: FnMut(&[u8]) -> Result<(), String>,
    {
        let command = CString::new(command).map_err(|_| "COPY command contains NUL".to_owned())?;
        let result = unsafe { PQexec(self.conn, command.as_ptr()) };
        if result.is_null() {
            return Err(self.error());
        }
        let status = unsafe { PQresultStatus(result) };
        if status != PGRES_COPY_OUT {
            let message = unsafe { CStr::from_ptr(PQresultErrorMessage(result)) }
                .to_string_lossy()
                .trim()
                .to_owned();
            unsafe { PQclear(result) };
            return Err(if message.is_empty() {
                self.error()
            } else {
                message
            });
        }
        unsafe { PQclear(result) };

        let mut binary_header_seen = false;
        loop {
            let mut buffer = std::ptr::null_mut();
            let length = unsafe { PQgetCopyData(self.conn, &raw mut buffer, 0) };
            if length > 0 {
                let mut bytes = unsafe {
                    std::slice::from_raw_parts(
                        buffer.cast::<u8>(),
                        usize::try_from(length).expect("positive COPY length"),
                    )
                };
                if !binary_header_seen {
                    bytes = match strip_copy_file_header(bytes) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            unsafe { PQfreemem(buffer.cast()) };
                            return Err(error);
                        }
                    };
                    binary_header_seen = true;
                    if bytes.is_empty() {
                        unsafe { PQfreemem(buffer.cast()) };
                        continue;
                    }
                }
                let result = match copy_binary_payload(bytes) {
                    Ok(Some(payload)) => sink(payload),
                    Ok(None) => Ok(()),
                    Err(error) => Err(error),
                };
                unsafe { PQfreemem(buffer.cast()) };
                // COPY OUT cannot be aborted with PQputCopyEnd (which is for COPY FROM).
                // Returning drops the connection and cancels the source COPY without draining it.
                result?;
            } else if length == -1 {
                break;
            } else {
                return Err(format!(
                    "PQgetCopyData failed ({}): {}",
                    length,
                    self.error()
                ));
            }
        }

        let result = unsafe { PQgetResult(self.conn) };
        if result.is_null() {
            return Err("COPY ended without a result".into());
        }
        let status = unsafe { PQresultStatus(result) };
        let message = if status == PGRES_COMMAND_OK {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(PQresultErrorMessage(result)) }
                    .to_string_lossy()
                    .trim()
                    .to_owned(),
            )
        };
        unsafe { PQclear(result) };
        if let Some(message) = message {
            return Err(message);
        }
        let trailing = unsafe { PQgetResult(self.conn) };
        if !trailing.is_null() {
            unsafe { PQclear(trailing) };
            return Err("unexpected extra COPY result".into());
        }
        Ok(())
    }
}

impl Drop for SourceClient {
    fn drop(&mut self) {
        unsafe { PQfinish(self.conn) }
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_binary_payload, ServerInfo};

    fn server_info() -> ServerInfo {
        ServerInfo {
            server_version_num: 170_004,
            block_size: 8192,
            max_align: 8,
            endianness: "little".to_owned(),
            server_encoding: "UTF8".to_owned(),
            data_checksums: true,
            extension_version: "0.3.0".to_owned(),
            max_toast_chunk_size: 1996,
            max_identifier_length: 63,
            checksum_algorithm_version: 1,
        }
    }

    #[test]
    fn accepts_same_major_with_different_minor() {
        let target = server_info();
        let source = ServerInfo {
            server_version_num: 170_005,
            ..target.clone()
        };
        assert!(source.ensure_compatible(&target).is_ok());
    }

    #[test]
    fn accepts_only_same_major_and_upward_pairs() {
        for (source_major, target_major) in [(16, 17), (16, 18), (17, 18)] {
            let target = ServerInfo {
                server_version_num: target_major * 10000,
                ..server_info()
            };
            let source = ServerInfo {
                server_version_num: source_major * 10000,
                ..target.clone()
            };
            assert!(source.ensure_compatible(&target).is_ok());
        }
    }

    #[test]
    fn rejects_downgrade_and_unsupported_major_pairs() {
        for (source_major, target_major) in [(17, 16), (18, 16), (18, 17), (15, 16), (16, 19)] {
            let target = ServerInfo {
                server_version_num: target_major * 10000,
                ..server_info()
            };
            let source = ServerInfo {
                server_version_num: source_major * 10000,
                ..target.clone()
            };
            let error = source.ensure_compatible(&target).unwrap_err();
            assert!(error.contains("PostgreSQL major"), "{error}");
        }
    }

    #[test]
    fn rejects_extension_mismatch() {
        let target = server_info();
        let source = ServerInfo {
            extension_version: "0.0.0".to_owned(),
            ..target.clone()
        };
        let error = source.ensure_compatible(&target).unwrap_err();
        assert!(error.contains("extension version"), "{error}");
    }

    #[test]
    fn rejects_physical_mismatches() {
        let target = server_info();
        let cases = [
            (
                "block size",
                ServerInfo {
                    block_size: 4096,
                    ..target.clone()
                },
            ),
            (
                "max align",
                ServerInfo {
                    max_align: 4,
                    ..target.clone()
                },
            ),
            (
                "endianness",
                ServerInfo {
                    endianness: "big".to_owned(),
                    ..target.clone()
                },
            ),
            (
                "server encoding",
                ServerInfo {
                    server_encoding: "SQL_ASCII".to_owned(),
                    ..target.clone()
                },
            ),
            (
                "data checksums",
                ServerInfo {
                    data_checksums: false,
                    ..target.clone()
                },
            ),
            (
                "TOAST chunk size",
                ServerInfo {
                    max_toast_chunk_size: 1024,
                    ..target.clone()
                },
            ),
            (
                "identifier length",
                ServerInfo {
                    max_identifier_length: 31,
                    ..target.clone()
                },
            ),
            (
                "checksum algorithm",
                ServerInfo {
                    checksum_algorithm_version: 2,
                    ..target.clone()
                },
            ),
        ];
        for (field, source) in cases {
            let error = source.ensure_compatible(&target).unwrap_err();
            assert!(error.contains(field), "{error}");
        }
    }

    #[test]
    fn extracts_bytea_from_copy_envelope() {
        let row = [0, 1, 0, 0, 0, 3, b'P', b'G', b'A'];
        assert_eq!(copy_binary_payload(&row).unwrap(), Some(&b"PGA"[..]));
        assert_eq!(copy_binary_payload(&[0xff, 0xff]).unwrap(), None);
    }

    #[test]
    fn rejects_malformed_copy_envelope() {
        assert!(copy_binary_payload(&[0, 1, 0, 0, 0, 4, b'P']).is_err());
        assert!(copy_binary_payload(&[0, 2, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn strips_binary_copy_file_header() {
        let mut bytes = b"PGCOPY\n\xff\r\n\0\0\0\0\0\0\0\0\0".to_vec();
        bytes.extend_from_slice(&[0, 1]);
        assert_eq!(super::strip_copy_file_header(&bytes).unwrap(), &[0, 1]);
        assert!(super::strip_copy_file_header(b"not a copy header").is_err());
    }
}
