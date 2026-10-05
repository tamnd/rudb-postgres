# pgx v5, which sends SELECT 1 through the extended flow.
set -e
if [ ! -x build/smoke ] || [ main.go -nt build/smoke ]; then
  [ -f go.sum ] || go get github.com/jackc/pgx/v5@v5.7.6 >/dev/null 2>&1
  go build -o build/smoke .
fi
build/smoke
