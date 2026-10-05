# libpq_pipeline sessions

Each file is the session of one test of `src/test/modules/libpq_pipeline` at the pin, recorded through the proxy from the oracle in the exact trace style. These are the 9 tests that have a trace file in the pin.

To record a file again, make the database `replay` new on the oracle, then run the proxy and the test:

```
rudb-postgres record --to oracle --name <test> --sessions 1 &
PGPASSWORD=postgres target/oracle/<pin>/test/pipeline/bin/libpq_pipeline <test> "host=127.0.0.1 port=55440 user=postgres dbname=replay sslmode=disable"
```

Then copy `run/traces/<test>-1.trace` here. `rudb-postgres replay` runs the files in a new database `replay` on each server.
