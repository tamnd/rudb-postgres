-- The smoke file of the differential runner. It touches each kind of answer once: rows in text
-- and binary, an empty query, errors with each optional field, notices, COPY in both directions,
-- transactions and an aborted transaction, and rows with and without an order.

select 1;
select 1 as a, 'x'::text as b, null::int as c, true as d;
select 1.5::numeric(10, 2), 2.5::float8, 'NaN'::float8, '-Infinity'::float4;
select '2026-10-05 12:34:56.789+02'::timestamptz, '2026-10-05'::date, '12:34'::time, '1 day 2 hours'::interval;
select '{1,2,NULL}'::int[], '{{a,b},{c,d}}'::text[], array[]::int[];
select '\x00ff'::bytea, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid, '{"a": [1, 2]}'::jsonb, '{"a": [1, 2]}'::json;
select 32767::int2, 2147483647::int4, 9223372036854775807::int8, 'abc'::char(5), 'abc'::varchar(2);
select '192.168.0.1/24'::inet, '[1,5)'::int4range, 'a & b'::tsquery, B'1010'::bit(4);
select row(1, 'a'), (1, 2)::record is not null;
select current_setting('TimeZone'), current_setting('DateStyle'), current_setting('server_encoding');

-- An empty query, and a statement that is only a comment.
;
/* nothing */;

create table t (id int primary key, name text not null, score numeric, created date default '2026-01-01');
comment on table t is 'the smoke table';
insert into t values (1, 'one', 1.5), (2, 'two', null), (3, 'three', 3.25);
insert into t (id, name) values (4, 'four') returning *;
update t set score = 0 where id = 2;
delete from t where id = 4;
select * from t;
select * from t order by id desc;
select name, count(*) over () from t order by name;
select score is null, sum(score) from t group by 1 order by 1;

-- COPY in both directions.
copy t (id, name, score) from stdin;
10	ten	10.5
11	eleven	\N
\.
copy t to stdout;
copy (select id, name from t order by id) to stdout with (format csv, header);
copy t (id, name) from stdin;
\.

-- Errors with each optional field: position, detail, hint, schema, table, column, constraint.
select * from missing_table;
select nosuchcolumn from t;
insert into t values (1, 'again', 0);
insert into t (id) values (20);
select 1 / 0;
select 'abc'::int;
select repeat('x', 3) +;
create table t (id int);

-- Notices.
drop table if exists not_there;
create table if not exists t (id int);
do $$ begin raise notice 'from a block: %', 42; end $$;
do $$ begin raise warning 'with detail' using detail = 'the detail', hint = 'the hint'; end $$;

-- Transactions, an aborted one, and a savepoint.
begin;
insert into t values (30, 'thirty', 30);
savepoint s;
select 1 / 0;
rollback to savepoint s;
select count(*) from t;
commit;
begin;
select missing;
select 1;
rollback;
begin isolation level serializable read only;
show transaction_isolation;
commit;

-- Settings.
set datestyle = 'German';
select '2026-10-05'::date;
reset datestyle;
set search_path = pg_catalog;
show search_path;
reset search_path;
set no_such_setting = 1;

-- Catalog reads that every tool does.
select typname, typlen, typbyval from pg_type where oid in (16, 20, 23, 25, 1043, 1184) order by oid;
select relname, relkind from pg_class where relname = 't';
select attname, atttypid::regtype, attnotnull from pg_attribute where attrelid = 't'::regclass and attnum > 0 order by attnum;
select obj_description('t'::regclass, 'pg_class');
select version() like 'PostgreSQL 19%';

drop table t;
