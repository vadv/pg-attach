use super::wire::{self, TableSchema};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

#[allow(non_camel_case_types)]
enum PGconn {}
#[allow(non_camel_case_types)]
enum PGresult {}

const CONNECTION_OK: c_int = 0;
const PGRES_COMMAND_OK: c_int = 1;
const PGRES_COPY_OUT: c_int = 3;

#[link(name = "pq")]
unsafe extern "C" {
    fn PQconnectdb(conninfo: *const c_char) -> *mut PGconn;
    fn PQstatus(conn: *const PGconn) -> c_int;
    fn PQerrorMessage(conn: *const PGconn) -> *const c_char;
    fn PQfinish(conn: *mut PGconn);
    fn PQexec(conn: *mut PGconn, query: *const c_char) -> *mut PGresult;
    fn PQresultStatus(result: *const PGresult) -> c_int;
    fn PQresultErrorMessage(result: *const PGresult) -> *const c_char;
    fn PQclear(result: *mut PGresult);
    fn PQgetCopyData(conn: *mut PGconn, buffer: *mut *mut c_char, async_: c_int) -> c_int;
    fn PQfreemem(pointer: *mut c_void);
    fn PQgetResult(conn: *mut PGconn) -> *mut PGresult;
}

pub struct SourceClient {
    conn: *mut PGconn,
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
        let conninfo =
            CString::new(conninfo).map_err(|_| "connection string contains NUL".to_owned())?;
        let conn = unsafe { PQconnectdb(conninfo.as_ptr()) };
        if conn.is_null() {
            return Err("PQconnectdb returned NULL".into());
        }
        let client = Self { conn };
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
    use super::copy_binary_payload;

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
