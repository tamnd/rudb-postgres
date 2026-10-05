# asyncpg, which speaks the protocol itself and sends SELECT 1 through the extended flow.
set -e
if [ ! -x .venv/bin/python ]; then
  uv venv --quiet --python 3.14 .venv
  uv pip install --quiet --python .venv/bin/python "asyncpg==0.31.0"
fi
.venv/bin/python smoke.py
