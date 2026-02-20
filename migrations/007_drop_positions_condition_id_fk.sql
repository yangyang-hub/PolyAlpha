-- Drop foreign key constraint on positions.condition_id
-- Directional strategy positions may reference markets not stored in the markets table,
-- causing silent FK violations during position persistence.
ALTER TABLE positions DROP CONSTRAINT IF EXISTS positions_condition_id_fkey;
