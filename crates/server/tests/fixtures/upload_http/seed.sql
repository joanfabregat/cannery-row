-- Finite, isolated serialization fixtures; lease authority remains actual PostgreSQL now().
UPDATE attempts SET state='claimed',claimed_by_user='00000000-0000-0000-0000-000000000002',claimed_by_service=NULL,via_channel='api',lease_generation=1,lease_token_hash=sha256(convert_to('cr_lease_fixture','UTF8')),lease_expires_at='2099-01-01Z',started_at=NULL,deadline=NULL,workflow=NULL WHERE id='00000000-0000-0000-0000-000000002001';
UPDATE api_tokens SET expires_at='2099-01-01Z';
INSERT INTO api_tokens(token_hash,display_prefix,kind,user_id,name,scopes,expires_at) VALUES(sha256(convert_to('cr_pat_track_http_researcher-copy','UTF8')),'cr_pat_fixture','personal','00000000-0000-0000-0000-000000000002','researcher-copy',ARRAY['read','write'],'2099-01-01Z');
UPDATE hypotheses SET state='active',approved_revision=revision,approved_at='2001-01-01Z' WHERE id='00000000-0000-0000-0000-000000001001';
UPDATE search_documents SET updated_at='2001-01-01Z';
INSERT INTO jobs(id,project_id,attempt_id,phase,run_number,science_revision,performer,verifier_id,spec,deadline_seconds,created_at) VALUES('00000000-0000-0000-0000-000000006001','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002001','verify',1,1,'runner','fixture','{}',60,'2001-01-01Z');
CREATE TABLE fixture_upload_profile(phase text);
INSERT INTO fixture_upload_profile VALUES('fixed');
CREATE FUNCTION fixture_upload_row() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE phase text;
BEGIN
SELECT f.phase INTO phase FROM fixture_upload_profile f;
IF phase='audit' AND TG_TABLE_NAME='audit_events' THEN RAISE EXCEPTION 'isolated upload audit failure'; END IF;
IF phase='fixed' OR phase='audit' THEN
IF TG_TABLE_NAME='uploads' THEN NEW.id='00000000-0000-0000-0000-000000090001';NEW.created_at='2001-01-01Z';IF NEW.receiving_since IS NOT NULL THEN NEW.receiving_since='2001-01-02Z';END IF;IF TG_OP='INSERT' THEN NEW.expires_at='2099-01-01Z';END IF;IF NEW.completed_at IS NOT NULL THEN NEW.completed_at='2001-01-02Z';END IF;IF NEW.urls_expire_at IS NOT NULL THEN NEW.urls_expire_at='2001-01-03Z';END IF;
ELSIF TG_TABLE_NAME='artifacts' THEN NEW.id='00000000-0000-0000-0000-000000090002';NEW.verified_at='2001-01-02Z';
ELSIF TG_TABLE_NAME='attempt_failures' THEN NEW.id='00000000-0000-0000-0000-000000090003';NEW.created_at='2001-01-02Z';
ELSIF TG_TABLE_NAME='review_cases' THEN NEW.id='00000000-0000-0000-0000-000000090004';NEW.opened_at='2001-01-02Z';
ELSIF TG_TABLE_NAME='audit_events' THEN NEW.occurred_at='2001-01-02Z';
ELSIF TG_TABLE_NAME='attempts' THEN IF NEW.started_at IS NOT NULL THEN NEW.started_at='2001-01-02Z';END IF;IF NEW.finished_at IS NOT NULL THEN NEW.finished_at='2001-01-02Z';END IF;
ELSIF TG_TABLE_NAME='hypotheses' THEN NEW.updated_at='2001-01-02Z';
ELSIF TG_TABLE_NAME='search_documents' THEN NEW.updated_at='2001-01-02Z';
END IF;
END IF;
RETURN NEW;
END $$;
CREATE TRIGGER fixture_upload_row BEFORE INSERT OR UPDATE ON uploads FOR EACH ROW EXECUTE FUNCTION fixture_upload_row();
CREATE TRIGGER fixture_artifact_row BEFORE INSERT ON artifacts FOR EACH ROW EXECUTE FUNCTION fixture_upload_row();
CREATE TRIGGER fixture_failure_row BEFORE INSERT ON attempt_failures FOR EACH ROW EXECUTE FUNCTION fixture_upload_row();
CREATE TRIGGER fixture_case_row BEFORE INSERT ON review_cases FOR EACH ROW EXECUTE FUNCTION fixture_upload_row();
CREATE TRIGGER fixture_audit_row BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_upload_row();
CREATE TRIGGER fixture_attempt_row BEFORE UPDATE ON attempts FOR EACH ROW EXECUTE FUNCTION fixture_upload_row();
CREATE TRIGGER fixture_hypothesis_row BEFORE UPDATE ON hypotheses FOR EACH ROW EXECUTE FUNCTION fixture_upload_row();
CREATE TRIGGER fixture_search_row BEFORE INSERT OR UPDATE ON search_documents FOR EACH ROW EXECUTE FUNCTION fixture_upload_row();
