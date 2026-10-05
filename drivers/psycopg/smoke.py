"""Connects with psycopg 3 and its own libpq, and prints the result of SELECT 1."""
import os

import psycopg

with psycopg.connect(
    host=os.environ["GATE_HOST"],
    port=os.environ["GATE_PORT"],
    user=os.environ["GATE_USER"],
    password=os.environ["GATE_PASSWORD"],
    dbname=os.environ["GATE_DATABASE"],
    sslmode="verify-full",
    sslrootcert=os.environ["GATE_ROOT_CERT"],
) as conn:
    if conn.pgconn.ssl_in_use != 1:
        raise SystemExit("the session is not on TLS")
    print(conn.execute("select 1").fetchone()[0])
