# Runs the pgx tests in the checkout and writes the events of `go test -json` to $RESULTS.
#
# The md5 and password tests log in as the roles that the harness made for those rules. The TLS
# test checks the certificate of the server against the root of oracle/certs. The tests for
# client certificates, OAuth, pgbouncer and CrateDB are skipped, because their variables are
# not set.

tcp="host=127.0.0.1 port=$PGPORT dbname=$PGDATABASE sslmode=disable"
export PGX_TEST_DATABASE="$tcp user=postgres password=postgres"
export PGX_TEST_TCP_CONN_STRING="$PGX_TEST_DATABASE"
export PGX_TEST_UNIX_SOCKET_CONN_STRING="host=$RPG_SOCKET port=$PGPORT user=postgres dbname=$PGDATABASE"
export PGX_TEST_SCRAM_PASSWORD_CONN_STRING="$PGX_TEST_DATABASE"
export PGX_TEST_MD5_PASSWORD_CONN_STRING="$tcp user=rpg_md5 password=rpg"
export PGX_TEST_PLAIN_PASSWORD_CONN_STRING="$tcp user=rpg_password password=rpg"
export PGX_TEST_TLS_CONN_STRING="host=localhost port=$PGPORT user=postgres password=postgres dbname=$PGDATABASE sslmode=verify-full sslrootcert=$RPG_ROOT/oracle/certs/root.crt"

go test -json -count=1 ./... > "$RESULTS"
