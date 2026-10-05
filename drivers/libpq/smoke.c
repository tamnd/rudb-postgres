/* Connects with libpq, checks that the session is on TLS, and prints the result of SELECT 1. */
#include <stdio.h>
#include <libpq-fe.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: smoke <conninfo>\n");
        return 2;
    }
    PGconn *conn = PQconnectdb(argv[1]);
    if (PQstatus(conn) != CONNECTION_OK) {
        fprintf(stderr, "%s", PQerrorMessage(conn));
        return 1;
    }
    if (!PQsslInUse(conn)) {
        fprintf(stderr, "the session is not on TLS\n");
        return 1;
    }
    PGresult *res = PQexec(conn, "select 1");
    if (PQresultStatus(res) != PGRES_TUPLES_OK) {
        fprintf(stderr, "%s", PQerrorMessage(conn));
        return 1;
    }
    printf("%s\n", PQgetvalue(res, 0, 0));
    PQclear(res);
    PQfinish(conn);
    return 0;
}
