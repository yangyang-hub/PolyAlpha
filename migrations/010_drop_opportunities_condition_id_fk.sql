-- Drop foreign key constraint on opportunities.condition_id
-- Live strategy executions may reference markets that were never backfilled into
-- the archival markets table, and historical trade persistence should not fail
-- just because market metadata is missing.
ALTER TABLE opportunities DROP CONSTRAINT IF EXISTS opportunities_condition_id_fkey;
