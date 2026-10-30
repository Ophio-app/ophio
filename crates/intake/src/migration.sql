-- SPDX-License-Identifier: Apache-2.0
BEGIN IMMEDIATE;
CREATE TABLE reports (
    id TEXT PRIMARY KEY, key TEXT NOT NULL, signature TEXT NOT NULL,
    received INTEGER NOT NULL, updated INTEGER NOT NULL, closed INTEGER,
    status TEXT NOT NULL, record TEXT NOT NULL, bundle TEXT,
    group_id TEXT NOT NULL, lease_until INTEGER
);
CREATE INDEX reports_signature ON reports(signature,closed);
CREATE INDEX reports_group ON reports(group_id);
CREATE INDEX reports_key ON reports(key,updated);
CREATE INDEX reports_queue ON reports(received,id);
CREATE INDEX reports_retention ON reports(closed);
CREATE TABLE replays(signature TEXT PRIMARY KEY, expires INTEGER NOT NULL);
CREATE INDEX replays_expiry ON replays(expires);
CREATE TABLE buckets(id TEXT PRIMARY KEY, tokens REAL NOT NULL, at INTEGER NOT NULL, expires INTEGER NOT NULL);
CREATE TABLE bans(key TEXT PRIMARY KEY);
CREATE TABLE known(signature TEXT PRIMARY KEY, issue TEXT NOT NULL);
CREATE TABLE releases(build TEXT PRIMARY KEY, published INTEGER NOT NULL, entry TEXT NOT NULL);
CREATE TABLE settings(name TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO settings VALUES('switches','{"pipeline_paused":false,"intake_closed":false}');
CREATE TABLE rejections(reason TEXT PRIMARY KEY, count INTEGER NOT NULL);
CREATE TABLE audit(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, operation TEXT NOT NULL, target TEXT NOT NULL);
PRAGMA user_version=1;
COMMIT;
