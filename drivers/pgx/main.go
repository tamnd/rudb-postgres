// Connects with pgx, which speaks the protocol itself, and prints the result of SELECT 1.
package main

import (
	"context"
	"fmt"
	"net/url"
	"os"

	"github.com/jackc/pgx/v5"
)

func main() {
	dsn := fmt.Sprintf("postgres://%s:%s@%s:%s/%s?sslmode=verify-full&sslrootcert=%s",
		url.QueryEscape(os.Getenv("GATE_USER")), url.QueryEscape(os.Getenv("GATE_PASSWORD")),
		os.Getenv("GATE_HOST"), os.Getenv("GATE_PORT"), os.Getenv("GATE_DATABASE"),
		url.QueryEscape(os.Getenv("GATE_ROOT_CERT")))
	ctx := context.Background()
	conn, err := pgx.Connect(ctx, dsn)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer conn.Close(ctx)
	if conn.PgConn().Conn() == nil {
		fmt.Fprintln(os.Stderr, "no connection")
		os.Exit(1)
	}
	var one int
	if err := conn.QueryRow(ctx, "select 1").Scan(&one); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println(one)
}
