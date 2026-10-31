use std::env;
use std::process::Command;

fn pg_config(program: &str, key: &str) -> String {
    let output = Command::new(program)
        .arg(key)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {program} {key}: {e}"));
    assert!(output.status.success(), "{program} {key} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn main() {
    println!("cargo:rerun-if-changed=src/page_shim.c");
    println!("cargo:rerun-if-env-changed=PGRX_PG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=PG_CONFIG");

    let program = env::var("PGRX_PG_CONFIG_PATH")
        .or_else(|_| env::var("PG_CONFIG"))
        .unwrap_or_else(|_| "pg_config".to_owned());
    cc::Build::new()
        .file("src/page_shim.c")
        .include(pg_config(&program, "--includedir-server"))
        .include(pg_config(&program, "--includedir"))
        .debug(true)
        .compile("pg_attach_page_shim");
}
