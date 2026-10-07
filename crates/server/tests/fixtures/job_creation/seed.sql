UPDATE uploads SET expires_at='2099-01-01Z'
WHERE id IN('00000000-0000-0000-0000-000000000203','00000000-0000-0000-0000-000000000204');
INSERT INTO hypotheses(id,project_id,number,track_id,state,title,created_by_user,approved_revision,approved_at)
SELECT ('00000000-0000-0000-0000-'||lpad((1000+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',i,'00000000-0000-0000-0000-000000000004','active','Creation fixture','00000000-0000-0000-0000-000000000001',1,'2024-01-02Z' FROM generate_series(24,33) i;
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation)
SELECT ('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',('00000000-0000-0000-0000-'||lpad((1000+i)::text,12,'0'))::uuid,1,'testing',1,1,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000001','api',0 FROM generate_series(24,33) i;
