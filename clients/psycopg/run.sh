# Runs the psycopg tests in the checkout and writes a JUnit file to $RESULTS.
#
# The tests use psycopg-binary, which has its own libpq, as most applications do. The virtual
# environment is made once, in the checkout. The packages are installed and not editable,
# because the source directory `psycopg` of the checkout would hide an editable install from
# the tests. The tests run in their file order, without pytest-randomly, so that the two servers
# get the same sequence.
#
# The tests find the server only in PSYCOPG_TEST_DSN. The other PG variables are removed,
# because some tests compare a connection string with the environment.
#
# These tests are out:
# - The tests for timing, for subprocesses, for slow paths and the flaky tests, because their
#   result depends on the machine.
# - The tests for type checking with mypy, because they do not use a server.
# - The `remote_closed` tests, because they test the client against pproxy, and pproxy cannot
#   listen on macOS ("Socket is already connected").
# - `test_generators.py::test_cancel`, because it waits for `count(*)`, which always returns a
#   row. So the cancel can come before the query starts, and then the test waits 180 seconds.

set -e
if [ ! -x .venv/bin/python ]; then
  uv venv --quiet --python 3.14 .venv
  uv pip install --quiet --python .venv/bin/python ./psycopg ./psycopg_pool \
    psycopg-binary==3.3.6 "anyio>=4.0" "pproxy>=2.7" "pytest>=8" dnspython
fi

export PSYCOPG_IMPL=binary
export PSYCOPG_TEST_DSN="host=127.0.0.1 port=$PGPORT user=postgres password=postgres dbname=$PGDATABASE sslmode=disable"
unset PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSSLMODE
.venv/bin/pytest tests -q -p no:randomly \
  -m "not slow and not timing and not subprocess and not flakey and not mypy" \
  -k "not remote_closed" \
  --deselect tests/test_generators.py::test_cancel \
  --ignore=tests/test_typing.py --ignore=tests/crdb \
  --junitxml="$RESULTS" || true
