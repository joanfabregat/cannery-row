CREATE FUNCTION fixture_job_clock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.created_at > '2002-01-01Z' AND NEW.created_at < '2090-01-01Z' THEN
    NEW.created_at := '2024-01-02T03:04:05.123456Z';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER fixture_job_clock BEFORE INSERT ON jobs
FOR EACH ROW EXECUTE FUNCTION fixture_job_clock();
