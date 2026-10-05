# The driver gate

Each directory here is one client of the PG1 gate in document 17 section 17.3 of the notes. `rudb-postgres drivers` runs `run.sh` of each client against the oracle and against the other server. The script must connect over TCP with TLS and SCRAM, run `SELECT 1` and print what the server returned.

The script gets these variables:

- `GATE_HOST` and `GATE_PORT`: the server. The host is `localhost`, because the server certificate holds that name.
- `GATE_USER`, `GATE_PASSWORD` and `GATE_DATABASE`: the role `rpg`, which logs in with `scram-sha-256` over TCP.
- `GATE_ROOT_CERT`: the root certificate in `oracle/certs`.
- `GATE_PG_BIN`: the directory of the oracle programs, which has `psql`, `pg_config` and libpq.

Each client checks the certificate and the host name, the same as `sslmode=verify-full`, so a server without TLS fails the gate. A client is good when its script ends with status 0 and prints `1`.

A script builds or installs its client the first time, in its own directory. The build output stays in that directory and git ignores it.
