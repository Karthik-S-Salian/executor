-- Drop the table first since it depends on the enum types
DROP TABLE IF EXISTS submissions;

-- Then drop the enum types
DROP TYPE IF EXISTS submission_status;
DROP TYPE IF EXISTS language;
