-- tern is local-only: no remote devices, no pairing. (Gateway pairing codes
-- are a separate table, dropped with the gateways.)
DROP TABLE IF EXISTS device_pairing_codes;
DROP TABLE IF EXISTS devices;
