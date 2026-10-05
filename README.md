# rudb-postgres

The PostgreSQL compatibility harness for [rudb](https://github.com/tamnd/rudb).

This repository measures how far rudb is from PostgreSQL 19 as a client sees it. It starts PostgreSQL and rudb as two servers, sends the same input to each, and compares the replies byte by byte under a fixed set of rules. It does not make rudb compatible. All engine, catalog, protocol and server code stays in rudb, and a fix for a difference is always a change to rudb.

The harness treats rudb as a server and nothing else. It does not link any rudb crate. It has its own frame reader, so a bug in rudb's codec cannot hide itself.

## Status

PG0, which builds the harness. The plan is tamnd/rudb#2488. rudb has no PostgreSQL server yet, so every number for rudb is zero. Until it has one, the harness compares the oracle with a second copy of the oracle, and that comparison must show zero differences.

## The oracle

The oracle is PostgreSQL built from the commit in `pins.toml`, which is `REL_19_STABLE` at `7d3d2db7` (19beta4). When the oracle and the PostgreSQL documentation disagree, the oracle is correct.

The build is a meson release build with no assertions. `oracle/build.sh` has the flags. It needs meson, ninja, bison, flex, perl, a C compiler and OpenSSL. Both servers use the settings in `oracle/postgresql.conf` and the rules in `oracle/pg_hba.conf`, and present the test certificates in `oracle/certs`.

## Commands

| Command | What it does |
| --- | --- |
| `rudb-postgres oracle build` | Builds PostgreSQL from the pin into `target/oracle/<pin>` |
| `rudb-postgres up [--rudb <binary>]` | Starts the oracle and the other server, on two ports |
| `rudb-postgres down` | Stops the servers that `up` started |
| `rudb-postgres connect` | Logs in to both servers in each way and compares the replies |
| `rudb-postgres record --to <server> [--port <n>] [--sessions <n>] [--regress]` | Starts the proxy on port 55440 in front of a server and writes a trace for each session to `run/traces`. The trace format is the format of libpq's `PQtrace`. The proxy has no TLS and answers `SSLRequest` with `N` |
| `rudb-postgres replay <trace>...` | Replays each trace against both servers in a new database and compares the replies step by step |
| `rudb-postgres diff <file.sql>` | Runs each statement on both servers, in the simple query and in the extended query with text and binary results, and compares the answers |
| `rudb-postgres gen --seed <n> --count <n>` | Generates statements and runs them through `diff` |
| `rudb-postgres client <name>` | Runs one client suite against both servers and applies its lists |
| `rudb-postgres regress [--accept] [<test>...]` | Runs the core regression suite with `pg_regress` from the pin on both servers, and compares the diffs of rudb with `corpus/regress` |
| `rudb-postgres isolation [--accept] [<spec>...]` | Runs the isolation specs with `pg_isolation_regress` from the pin on both servers, and compares the diffs of rudb with `corpus/isolation` |
| `rudb-postgres report` | Writes the report page from the JSON files of one run |

A command that is not written yet says so and exits with code 2.

## How to reproduce a difference

1. Run `rudb-postgres oracle build`.
2. Run `rudb-postgres up --rudb <binary>` with a rudb binary from the commit in the report.
3. Run `rudb-postgres replay <case>` or `rudb-postgres diff <case>`.
4. Read the first line that differs in the output.

## License

Apache-2.0.
