# Secure Code Execution Engine in Rust

A sandboxed code-execution service for online judges and assessment tools. Untrusted
submissions are queued through NATS JetStream, executed by a worker pool using
[`isolate`](https://github.com/ioi/isolate), and judged against expected output.
Submissions and results live in Postgres.

## Architecture

![architecture diagram](architecture.png)

Crates in one Cargo workspace:

| Crate | Responsibility |
|---|---|
| `api` | HTTP + OpenAPI: create/read submissions, list languages, requeue lost jobs |
| `worker` | Pulls jobs, compiles/runs them in isolate, writes results back to Postgres |
| `common` | Postgres pool, NATS client/consumer, shared models |
| `sandbox` | Rootless Linux confinement library (namespaces, cgroup v2, seccomp, Landlock); not wired into the worker yet |

The checked-in worker still uses `isolate`. The `sandbox` crate is the intended
replacement; it is cross-compiled but must be validated on a real Linux worker
(first with `sandbox-probe`, then with its ignored integration tests) before it
takes traffic. See [`sandbox/README.md`](sandbox/README.md) for the host
requirements and the exact verification commands.

Flow: `POST /submissions/new` inserts a row (`inqueue`) → publishes to NATS
`submission.new` → a worker claims the row atomically (`processing`) → isolate
`--init`, write source/stdin, optional compile, run (possibly N times) → status,
output and resource usage are written back → message acked. Failed jobs are
`nak`ed for redelivery (max 5 attempts). Rows stuck in `inqueue` for 60s are
re-published by the API's sweeper; atomic claiming makes duplicates harmless.

## Requirements

- Rust stable (2024 edition)
- Linux host for the worker with:
  - [`isolate`](https://github.com/ioi/isolate) installed (`sudo apt install isolate`)
  - the compilers/interpreters named in your language config
  - permission to create cgroups (run as a systemd service or root; no `sudo` wrapper)
- Postgres 14+ and NATS 2.10+ with JetStream enabled

Development dependencies (Postgres + JetStream NATS):

```bash
docker compose up -d
export DATABASE_URL=postgres://executor:executor@localhost:5432/executor
export NATS_URL=nats://localhost:4222
```

## Build & Run

```bash
cargo build --release

# terminal 1 — API (applies the schema on startup, creates the NATS stream)
cargo run -p api

# terminal 2 — worker (Linux + isolate)
cargo run -p worker
```

The schema in `sql/up.sql` is applied automatically on API **and** worker startup
(idempotent). For real deployments, run it as a versioned migration instead.

## Environment variables

| Variable | Used by | Default |
|---|---|---|
| `DATABASE_URL` | api, worker | required |
| `NATS_URL` | api, worker | `nats://localhost:4222` |
| `BIND_ADDR` | api | `0.0.0.0:3000` |
| `EXECUTOR_CONFIG_PATH` | api | `config.toml` |
| `WORKER_CONFIG_PATH` | worker | `config.toml` |
| `ISOLATE_BIN` | worker | `isolate` |

Worker config (`WORKER_CONFIG_PATH`) supports `num_workers`; it defaults to the
number of CPUs. API config holds languages and submission defaults — see
`api/config.toml`.

## API

Swagger UI: `http://localhost:3000/docs`, spec: `http://localhost:3000/openapi.json`,
health: `GET /health`.

### `POST /submissions/new`

```bash
curl -X POST http://localhost:3000/submissions/new \
  -H 'Content-Type: application/json' \
  -d '{
    "source_code": "print(\"hello world\")",
    "language": "python",
    "stdin": "",
    "expected_output": "hello world",
    "cpu_time_limit": 1,
    "memory_limit": 128000,
    "number_of_runs": 1
  }'
```

Response: `{"id": "<uuid>"}`. Missing limits are filled from
`submission_defaults` in `api/config.toml`.

Not supported yet (rejected with `400`): `additional_files`, `callback_url`,
per-process time/memory limits. Network access is only allowed when both the
request and the language config allow it.

### `GET /submissions/:id`

Returns the submission with its current status and, once finished,
`stdout`, `stderr`, `compile_output`, `exit_code`, `exit_signal`, `time`,
`wall_time`, `memory`, `message`, `finished_at`.

### `GET /languages/`

Returns configured language names.

## Statuses

`inqueue`, `processing`, `accepted`, `wronganswer`, `timelimitexceeded`,
`compilationerror`, `runtimeerrorsigsegv`, `runtimeerrorsigxfsz`,
`runtimeerrorsigfpe`, `runtimeerrorsigabrt`, `runtimeerrornzec`,
`runtimeerrorother`, `internalerror`, `execformaterror`.

## Language configuration

```toml
[[languages]]
name = "c"
source_file = "main.c"
file_extension = "c"
compile_cmd = "gcc -o main main.c"
run_cmd = "./main"
allow_network = false
```

`compile_cmd`/`run_cmd` can also be read from files with `compile_cmd_file` /
`run_cmd_file`. User-supplied `compiler_options` and `command_line_arguments` are
split into arguments and shell-quoted before being placed in the sandbox script.

## Security notes

- Each run gets a fresh isolate box (namespaces + cgroups) with CPU, wall time,
  memory, stack, file size and process limits; network is off unless requested
  and allowed by the language config.
- The worker must run with the privileges isolate needs. Keep the API off the
  sandbox host, or at least off the privileged user.
- User options are quoted, not interpolated raw, but the sandbox remains the
  security boundary: do not treat `isolate` as protection against kernel exploits.
- `callback_url` is not implemented; no outbound HTTP is made.

## Testing

```bash
cargo test --workspace     # no Postgres/NATS/isolate needed
cargo check --workspace
```

## License

MIT
