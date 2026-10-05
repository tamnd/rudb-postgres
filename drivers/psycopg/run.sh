# psycopg 3 with psycopg-binary, which has its own libpq, as most applications do.
set -e
if [ ! -x .venv/bin/python ]; then
  uv venv --quiet --python 3.14 .venv
  uv pip install --quiet --python .venv/bin/python "psycopg[binary]==3.3.6"
fi
.venv/bin/python smoke.py
