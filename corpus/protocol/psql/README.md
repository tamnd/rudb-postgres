# psql sessions

Each file is one psql session, recorded through the proxy from the oracle in the exact trace style. The files test the two replay rules that a recorded session needs on a new server.

| File | What it tests |
| --- | --- |
| `describe.trace` | `\d rp_t`. psql reads the OID of the table and sends it in the next queries, so the replay must map the recorded OID to the OID of each server |
| `cancel.trace` | `select pg_sleep(5)` and a cancel from Ctrl+C. The proxy writes the `CancelRequest` in this session, and the replay sends it with the key of its own session |

To record the files again, run the proxy and psql from the pin:

```
export PGPASSWORD=postgres C="host=127.0.0.1 port=55440 user=postgres dbname=postgres sslmode=disable"
rudb-postgres record --to oracle --name describe --sessions 1 &
psql -X "$C" -c "create table rp_t(a int)" -c "\d rp_t" -c "drop table rp_t"
rudb-postgres record --to oracle --name cancel --sessions 2 &
psql -X "$C" -c "select pg_sleep(5)" & sleep 1; kill -INT $!
```

The cancel is the second session of the proxy, and it goes to the trace of the first. Then copy `run/traces/describe-1.trace` and `run/traces/cancel-1.trace` here.
