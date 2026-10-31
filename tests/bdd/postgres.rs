use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;
use tempfile::TempDir;
use tokio_postgres::{Client, NoTls, SimpleQueryMessage};

#[derive(Debug)]
pub struct PostgresPair {
    source: Postgres,
    target: Postgres,
    temp: TempDir,
}

impl PostgresPair {
    pub async fn start() -> Result<Self, String> {
        Self::start_with_checksums(false).await
    }

    pub async fn start_with_checksums(checksums: bool) -> Result<Self, String> {
        let temp = TempDir::new().map_err(|error| error.to_string())?;
        if is_root() {
            run(
                "chown",
                &["postgres:postgres", &temp.path().to_string_lossy()],
                false,
            )?;
        }
        let source = Postgres::start(temp.path(), "source", checksums).await?;
        let target = Postgres::start(temp.path(), "target", checksums).await?;
        Ok(Self {
            source,
            target,
            temp,
        })
    }

    pub fn source_port(&self) -> u16 {
        self.source.port
    }

    pub fn target_port(&self) -> u16 {
        self.target.port
    }

    fn port(&self, side: &str) -> u16 {
        match side {
            "source" => self.source.port,
            "target" => self.target.port,
            _ => panic!("unknown PostgreSQL side: {side}"),
        }
    }

    pub async fn execute(&self, side: &str, sql: &str) -> Result<(), String> {
        execute(self.port(side), sql).await
    }

    pub async fn query(&self, side: &str, sql: &str) -> Result<String, String> {
        query(self.port(side), sql).await
    }

    pub async fn create_tablespace(&self, side: &str) -> Result<(), String> {
        let path = self.temp.path().join("tablespace");
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        if is_root() {
            run(
                "chown",
                &["postgres:postgres", &path.to_string_lossy()],
                false,
            )?;
        }
        self.execute(
            side,
            &format!(
                "CREATE TABLESPACE attach_space LOCATION '{}'",
                path.to_string_lossy()
            ),
        )
        .await
    }

    pub fn stop_source(&self) -> Result<(), String> {
        self.source.stop()
    }

    pub async fn cancel_target(&self, sql: &str, delay: Duration) -> Result<(), String> {
        cancel_execute(self.target.port, sql, delay).await
    }
}

#[derive(Debug)]
struct Postgres {
    data: PathBuf,
    log: PathBuf,
    port: u16,
}

impl Postgres {
    async fn start(parent: &Path, name: &str, checksums: bool) -> Result<Self, String> {
        let data = parent.join(name);
        let log = parent.join(format!("{name}.log"));
        let port = unused_port()?;
        let data_arg = data.to_string_lossy().into_owned();
        let mut initdb_args = vec![
            "-D",
            &data_arg,
            "--no-sync",
            "--auth=trust",
            "--username=postgres",
        ];
        if checksums {
            initdb_args.push("--data-checksums");
        }
        run("initdb", &initdb_args, true)?;
        if !checksums {
            // PG18 enables checksums by default; older versions report already disabled.
            let _ = run("pg_checksums", &["-D", &data_arg, "--disable"], true);
        }
        let socket = parent.to_string_lossy();
        let options = format!("-F -p {port} -k {socket} -c listen_addresses=127.0.0.1");
        run(
            "pg_ctl",
            &[
                "-D",
                &data_arg,
                "-l",
                &log.to_string_lossy(),
                "-w",
                "-o",
                &options,
                "start",
            ],
            true,
        )?;
        let postgres = Self { data, log, port };
        postgres.wait_until_ready().await?;
        Ok(postgres)
    }

    fn stop(&self) -> Result<(), String> {
        run(
            "pg_ctl",
            &[
                "-D",
                &self.data.to_string_lossy(),
                "-w",
                "-m",
                "immediate",
                "stop",
            ],
            true,
        )
    }

    async fn wait_until_ready(&self) -> Result<(), String> {
        for _ in 0..100 {
            if query(self.port, "SELECT 1").await.as_deref() == Ok("1") {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(format!(
            "PostgreSQL on port {} did not become ready: {}",
            self.port,
            std::fs::read_to_string(&self.log).unwrap_or_default()
        ))
    }
}

impl Drop for Postgres {
    fn drop(&mut self) {
        let _ = self.stop();
        if std::thread::panicking() {
            eprintln!(
                "PostgreSQL log {}:\n{}",
                self.log.display(),
                std::fs::read_to_string(&self.log).unwrap_or_default()
            );
        }
    }
}

fn error_detail(error: &tokio_postgres::Error) -> String {
    error.as_db_error().map_or_else(
        || error.to_string(),
        |db| format!("{} {}: {}", db.severity(), db.code().code(), db.message()),
    )
}

async fn connect(port: u16) -> Result<(Client, tokio::task::JoinHandle<()>), String> {
    let dsn = format!("host=127.0.0.1 port={port} dbname=postgres user=postgres");
    let (client, connection) = tokio_postgres::connect(&dsn, NoTls)
        .await
        .map_err(|error| error.to_string())?;
    let task = tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("PostgreSQL connection error: {error}");
        }
    });
    Ok((client, task))
}

async fn cancel_execute(port: u16, sql: &str, delay: Duration) -> Result<(), String> {
    let (client, task) = connect(port).await?;
    let cancel = client.cancel_token();
    let result = {
        let execution = client.batch_execute(sql);
        tokio::pin!(execution);
        tokio::select! {
            result = &mut execution => {
                return Err(format!("import finished before cancellation: {result:?}"));
            }
            () = tokio::time::sleep(delay) => {
                cancel.cancel_query(NoTls).await.map_err(|error| error.to_string())?;
                execution.await
            }
        }
    };
    drop(client);
    let _ = task.await;
    match result {
        Ok(()) => Err("cancelled import unexpectedly succeeded".to_string()),
        Err(_) => Ok(()),
    }
}

async fn execute(port: u16, sql: &str) -> Result<(), String> {
    let (client, task) = connect(port).await?;
    let result = client
        .batch_execute(sql)
        .await
        .map_err(|error| error_detail(&error));
    drop(client);
    let _ = task.await;
    result
}

async fn query(port: u16, sql: &str) -> Result<String, String> {
    let (client, task) = connect(port).await?;
    let result = async {
        let messages = client
            .simple_query(sql)
            .await
            .map_err(|error| error_detail(&error))?;
        let mut rows = Vec::new();
        for message in messages {
            if let SimpleQueryMessage::Row(row) = message {
                let values = (0..row.len())
                    .map(|column| row.get(column).unwrap_or("NULL"))
                    .collect::<Vec<_>>()
                    .join("|");
                rows.push(values);
            }
        }
        Ok(rows.join("\n"))
    }
    .await;
    drop(client);
    let _ = task.await;
    result
}

fn unused_port() -> Result<u16, String> {
    TcpListener::bind("127.0.0.1:0")
        .map_err(|error| error.to_string())?
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

fn is_root() -> bool {
    static ROOT: OnceLock<bool> = OnceLock::new();
    *ROOT.get_or_init(|| {
        Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|output| output.stdout == b"0\n")
    })
}

fn run(program: &str, args: &[&str], as_postgres: bool) -> Result<(), String> {
    let mut command = if as_postgres && is_root() {
        let mut command = Command::new("runuser");
        command.args(["-u", "postgres", "--", program]);
        command
    } else {
        Command::new(program)
    };
    let output = command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with {}", output.status))
    }
}
