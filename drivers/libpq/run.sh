# A C program on the libpq of the oracle build.
set -e
include=$("$GATE_PG_BIN/pg_config" --includedir)
lib=$("$GATE_PG_BIN/pg_config" --libdir)
if [ ! -x build/smoke ] || [ smoke.c -nt build/smoke ]; then
  mkdir -p build
  cc -O1 -I"$include" -o build/smoke smoke.c -L"$lib" -lpq -Wl,-rpath,"$lib"
fi
PGPASSWORD="$GATE_PASSWORD" build/smoke \
  "host=$GATE_HOST port=$GATE_PORT user=$GATE_USER dbname=$GATE_DATABASE sslmode=verify-full sslrootcert=$GATE_ROOT_CERT"
