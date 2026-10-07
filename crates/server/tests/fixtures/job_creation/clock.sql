CREATE TABLE creation_clocks(id uuid PRIMARY KEY, clock timestamptz NOT NULL);
CREATE FUNCTION observe_creation_claim() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF OLD.state='pending' AND NEW.state='claimed' THEN
    INSERT INTO creation_clocks VALUES(NEW.id,now());
  END IF;
  RETURN NULL;
END $$;
CREATE TRIGGER observe_creation_claim AFTER UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION observe_creation_claim();
