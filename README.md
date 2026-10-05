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
| `rudb-postgres record --to <server> [--port <n>] [--sessions <n>] [--regress]` | Starts the proxy on port 55440 in front of a server and writes a trace for each session to `run/traces`. The trace format is the format of libpq's `PQtrace`. The proxy has no TLS and answers `SSLRequest` with `N`. A `CancelRequest` goes to the trace of the session that it cancels |
| `rudb-postgres replay <trace>...` | Replays each trace against both servers in a new database and compares the replies step by step. It maps each recorded user OID to the OID of each server, and sends a recorded cancel with the key of its own session |
| `rudb-postgres diff <file.sql>` | Runs each statement on both servers, in the simple query and in the extended query with text and binary results, and compares the answers |
| `rudb-postgres gen [--seed <n>] [--count <n>]` | Generates a file from a seed: the edge values of each type under each output setting, calls to each immutable function of `pg_catalog` with edge values, and random queries over three tables. It runs the file through `diff`, then runs TLP and NoREC logic checks on each server alone. Without `--seed`, the seed comes from the clock and the output gives it |
| `rudb-postgres client [--accept] [<name>...]` | Runs the test suite of each client in `clients`, or of the named clients, on both servers. The checkout of the pinned tag goes to `target/clients`. A test that fails on the oracle must be in `oracle-fail.txt`, and a test that passes on the oracle and fails on rudb must be in `expected-fail.txt`. Any other failure, and any pass of a listed test, is a difference. `--accept` writes the two lists |
| `rudb-postgres regress [--accept] [<test>...]` | Runs the core regression suite with `pg_regress` from the pin on both servers, and compares the diffs of rudb with `corpus/regress` |
| `rudb-postgres isolation [--accept] [<spec>...]` | Runs the isolation specs with `pg_isolation_regress` from the pin on both servers, and compares the diffs of rudb with `corpus/isolation` |
| `rudb-postgres report [--rudb [<commit>]] [--date <yyyy-mm-dd>]` | Reads the result files in `run/results` and counts the passed cases of each denominator of the notes. With `--rudb`, it writes the page of that rudb commit, or of the commit in `pins.toml`, to `reports/<date>/<commit>.md` and `.json`, and only counts the results that name rudb as the other server. Without it, it writes the page of the twin to `run/report`. The page gives the cases that changed state since the page before it |

A command that is not written yet says so and exits with code 2.

## Client suites

Each directory in `clients` pins one client at a tag and its commit in `client.toml`. `run.sh` runs the suite of the client in its checkout, and finds the server in the `PG` variables that the harness sets. `setup.sql` makes the objects that the suite expects in its new database. The suite of pgx needs Go, and the suite of psycopg needs uv.

| Client | Tag | Tests that pass on the oracle | Why the others fail on the oracle |
| --- | --- | ---: | --- |
| pgx | v5.11.0 | 2458 of 2463 | The oracle is built without libxml |
| psycopg | 3.3.6 | 5089 of 5104 | PostgreSQL 19 removed `standard_conforming_strings = off` and `escape_string_warning` |

## How to reproduce a difference

1. Run `rudb-postgres oracle build`.
2. Run `rudb-postgres up --rudb <binary>` with a rudb binary from the commit in the report.
3. Run `rudb-postgres replay <case>` or `rudb-postgres diff <case>`.
4. Read the first line that differs in the output.

## License

Apache-2.0.
