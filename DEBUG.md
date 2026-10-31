# Debugging the PoC

The compose services run with `SYS_PTRACE` and an unconfined seccomp profile. The
runtime image includes `gdb`, `strace`, `procps`, `lsof` and `binutils`; the
extension release build retains Rust and C debug symbols.

Find a busy import backend and capture a native backtrace:

```sh
docker compose exec -T target ps -ef
docker compose exec -T target gdb -p <PID> -batch \
  -ex 'set pagination off' -ex 'thread apply all bt full'
```

For a syscall trace (stop it with Ctrl-C):

```sh
docker compose exec -T target strace -ff -p <PID> -o /tmp/pg-attach.strace
```

The extension retains debug symbols, so native backtraces resolve into the
Rust importer/exporter and the C shim. Temporary logging can be added around
those functions when a failure needs a more detailed trace.
