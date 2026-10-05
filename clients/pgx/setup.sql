-- The objects that the pgx tests expect, from testsetup/postgresql_setup.sql of pgx. The roles of
-- the harness take the place of the pgx roles. A role is part of the cluster and not of the
-- database, so the setup makes it only once.
create extension hstore;
create extension ltree;
create domain uint64 as numeric(20,0);
do $$
begin
  if not exists (select from pg_roles where rolname = ' tricky, '' } " \ test user ') then
    create user " tricky, ' } "" \ test user " superuser password 'secret';
  end if;
end
$$;
