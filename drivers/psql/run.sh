# psql from the oracle build, through the libpq of the same build.
set -e
PGPASSWORD="$GATE_PASSWORD" "$GATE_PG_BIN/psql" -X -A -t -c "select 1" \
  "host=$GATE_HOST port=$GATE_PORT user=$GATE_USER dbname=$GATE_DATABASE sslmode=verify-full sslrootcert=$GATE_ROOT_CERT"
