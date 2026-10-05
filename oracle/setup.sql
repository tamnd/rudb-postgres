-- The roles of document 14 section 14.3 of the notes. `up` runs this file once, over the Unix
-- socket, on a new data directory. The passwords are test values and are not secret.

ALTER ROLE postgres PASSWORD 'postgres';
CREATE ROLE rpg LOGIN PASSWORD 'rpg';
CREATE ROLE rpg_password LOGIN PASSWORD 'rpg';

-- An md5 rule needs a password that is stored as an md5 hash.
SET password_encryption = 'md5';
CREATE ROLE rpg_md5 LOGIN PASSWORD 'rpg';
RESET password_encryption;
