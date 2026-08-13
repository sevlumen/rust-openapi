CREATE TABLE users (
    id BIGINT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL,
    active BOOLEAN NOT NULL
);

INSERT INTO users (id, name, email, active)
SELECT id,
       'User ' || id,
       'user' || id || '@example.test',
       (id % 2 = 0)
FROM generate_series(1, 100) AS id;
