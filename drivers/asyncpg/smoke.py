"""Connects with asyncpg, which has its own protocol code, and prints the result of SELECT 1."""
import asyncio
import os
import ssl


async def main():
    context = ssl.create_default_context(cafile=os.environ["GATE_ROOT_CERT"])
    conn = await asyncpg.connect(
        host=os.environ["GATE_HOST"],
        port=int(os.environ["GATE_PORT"]),
        user=os.environ["GATE_USER"],
        password=os.environ["GATE_PASSWORD"],
        database=os.environ["GATE_DATABASE"],
        ssl=context,
    )
    try:
        if conn.get_server_pid() is None:
            raise SystemExit("no backend key data")
        print(await conn.fetchval("select 1"))
    finally:
        await conn.close()


import asyncpg  # noqa: E402

asyncio.run(main())
