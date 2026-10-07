INSERT INTO hypotheses(id,project_id,number,track_id,state,title,created_by_user,approved_revision,approved_at)
SELECT ('00000000-0000-0000-0000-'||lpad((1000+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',i,'00000000-0000-0000-0000-000000000004','active','Mutation fixture','00000000-0000-0000-0000-000000000001',1,'2024-01-02Z' FROM generate_series(700,905) i WHERE i BETWEEN 700 AND 723 OR i IN (799,900,901,902);
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation)
SELECT ('00000000-0000-0000-0000-'||lpad((2000+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',('00000000-0000-0000-0000-'||lpad((1000+i)::text,12,'0'))::uuid,1,'testing',1,1,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000001','api',0 FROM generate_series(700,905) i WHERE i BETWEEN 700 AND 723 OR i IN (799,900,901,902);
INSERT INTO jobs(id,project_id,attempt_id,stage,run_number,state,science_revision,tester_id,spec,deadline_seconds,created_at,logs,claimed_by_service,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline)
SELECT ('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',('00000000-0000-0000-0000-'||lpad((2000+i)::text,12,'0'))::uuid,'tester',1,'claimed',1,'fixture',CASE WHEN i IN (900,902) THEN ('{"n":'||repeat('9',4301)||'}')::jsonb ELSE '{"é":[true,null,1.0]}'::jsonb END,600,CASE WHEN i IN (900,901) THEN '10000-01-01Z'::timestamptz ELSE '2024-01-02Z'::timestamptz END,CASE WHEN i=901 THEN ('{"n":'||repeat('9',4301)||'}')::jsonb ELSE '[]'::jsonb END,'00000000-0000-0000-0000-000000000003',1,decode(lpad(i::text,64,'0'),'hex'),'2024-01-02Z','2024-01-02Z','2099-01-01Z' FROM generate_series(700,905) i WHERE i BETWEEN 700 AND 723 OR i IN (799,900,901,902);
CREATE TABLE mutation_clocks(id uuid PRIMARY KEY, lease_clock timestamptz, finish_clock timestamptz);
CREATE FUNCTION observe_mutation_clock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.lease_expires_at IS DISTINCT FROM OLD.lease_expires_at OR NEW.finished_at IS DISTINCT FROM OLD.finished_at THEN
    INSERT INTO mutation_clocks VALUES(NEW.id,CASE WHEN NEW.lease_expires_at IS DISTINCT FROM OLD.lease_expires_at THEN now() END,CASE WHEN NEW.finished_at IS DISTINCT FROM OLD.finished_at THEN now() END)
    ON CONFLICT(id) DO UPDATE SET lease_clock=CASE WHEN NEW.lease_expires_at IS DISTINCT FROM OLD.lease_expires_at THEN now() ELSE mutation_clocks.lease_clock END,finish_clock=CASE WHEN NEW.finished_at IS DISTINCT FROM OLD.finished_at THEN now() ELSE mutation_clocks.finish_clock END;
  END IF;
  RETURN NULL;
END $$;
CREATE TRIGGER observe_mutation_clock AFTER UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION observe_mutation_clock();
