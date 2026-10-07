-- Read side: comments with their edit history, the search index, and
-- measurements as queryable rows. The search index and the measurement rows
-- are projections maintained by triggers on the records they come from, so
-- every write path (API, background sweeps, built-in evaluator) feeds them
-- in the same transaction. pg_trgm comes from 0001.

-- Comments on a hypothesis (attempt_id NULL) or on one of its attempts.
-- Discussion, not scientific record: a comment never changes a state. Only
-- its body and revision change; every body is kept in comment_revisions.
CREATE TABLE comments (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id    uuid NOT NULL REFERENCES projects (id),
    hypothesis_id uuid NOT NULL REFERENCES hypotheses (id),
    attempt_id    uuid REFERENCES attempts (id),
    author_user   uuid NOT NULL REFERENCES users (id),
    body_markdown text NOT NULL CHECK (body_markdown ~ '\S'),
    revision      integer NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at    timestamptz NOT NULL DEFAULT now(),
    edited_at     timestamptz,
    CHECK ((revision = 1) = (edited_at IS NULL))
);
CREATE INDEX comments_hypothesis_idx ON comments (hypothesis_id, created_at, id);
CREATE INDEX comments_attempt_idx ON comments (attempt_id, created_at, id)
    WHERE attempt_id IS NOT NULL;

CREATE FUNCTION comments_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'comments are never deleted';
    END IF;
    IF (NEW.id, NEW.project_id, NEW.hypothesis_id, NEW.attempt_id, NEW.author_user,
        NEW.created_at)
       IS DISTINCT FROM
       (OLD.id, OLD.project_id, OLD.hypothesis_id, OLD.attempt_id, OLD.author_user,
        OLD.created_at) THEN
        RAISE EXCEPTION 'only a comment''s body changes';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'an edit is the next revision of the comment';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER comments_guarded BEFORE UPDATE OR DELETE ON comments
    FOR EACH ROW EXECUTE FUNCTION comments_guard();

CREATE TABLE comment_revisions (
    comment_id    uuid NOT NULL REFERENCES comments (id),
    revision      integer NOT NULL CHECK (revision > 0),
    body_markdown text NOT NULL CHECK (body_markdown ~ '\S'),
    via_channel   text NOT NULL,
    via_client    text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (comment_id, revision)
);
CREATE TRIGGER comment_revisions_immutable BEFORE UPDATE OR DELETE ON comment_revisions
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Reports are the claimed result sheets' report fields; list them per project.
CREATE INDEX evidence_records_project_stage_idx
    ON evidence_records (project_id, stage, created_at, id);

-- An attempt's result decisions, read for search hits and reports.
CREATE INDEX review_cases_attempt_kind_idx ON review_cases (attempt_id, kind);

-- Every string value of a JSON document, one per line.
CREATE FUNCTION json_text(doc jsonb) RETURNS text LANGUAGE sql IMMUTABLE AS $$
    SELECT coalesce(string_agg(v #>> '{}', E'\n'), '')
    FROM jsonb_path_query(doc, 'strict $.**') AS v
    WHERE jsonb_typeof(v) = 'string'
$$;

-- Search index: one document per searchable record. Full text uses the
-- language-neutral `simple` configuration, which suits mixed French/English
-- text; trigram indexes serve fuzzy matching. Artifact contents are never
-- indexed. A document's track, states, verdict and decision are read at query
-- time from the records it belongs to, so they are never stale.
CREATE TABLE search_documents (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id    uuid NOT NULL REFERENCES projects (id),
    kind          text NOT NULL CHECK (kind IN (
                      'track', 'hypothesis', 'attempt', 'report', 'tester_observation',
                      'evaluator_reason', 'decision_reason', 'comment')),
    source_id     uuid NOT NULL,
    track_id      uuid REFERENCES tracks (id),
    hypothesis_id uuid REFERENCES hypotheses (id),
    attempt_id    uuid REFERENCES attempts (id),
    actor_user    uuid REFERENCES users (id),
    actor_service uuid REFERENCES service_accounts (id),
    title         text NOT NULL DEFAULT '',
    body          text NOT NULL DEFAULT '',
    occurred_at   timestamptz NOT NULL,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    tsv           tsvector GENERATED ALWAYS AS (
                      setweight(to_tsvector('simple', title), 'A')
                      || setweight(to_tsvector('simple', body), 'B')) STORED,
    UNIQUE (kind, source_id),
    CHECK ((kind = 'track') = (track_id IS NOT NULL)),
    CHECK ((kind = 'track') = (hypothesis_id IS NULL))
);
CREATE INDEX search_documents_tsv_idx ON search_documents USING gin (tsv);
CREATE INDEX search_documents_title_trgm_idx ON search_documents USING gin (title gin_trgm_ops);
CREATE INDEX search_documents_body_trgm_idx ON search_documents USING gin (body gin_trgm_ops);
CREATE INDEX search_documents_project_idx ON search_documents (project_id, id);
CREATE INDEX search_documents_hypothesis_idx ON search_documents (hypothesis_id);
CREATE INDEX search_documents_attempt_idx ON search_documents (attempt_id);

-- A document indexes the first 2000 characters of its title and the first
-- 200000 of its body; the whole record stays in its own table. The bound
-- keeps every write indexable: to_tsvector fails ("string is too long for
-- tsvector", program_limit_exceeded) once a vector's lexemes and positions
-- pass 1048575 bytes, and a trigger failing that way would fail its write
-- (a submission, a job completion) every time it is retried. A distinct
-- lexeme costs its bytes plus at most 5 (alignment, position count, first
-- position), and later positions stop at 16383, so ordinary prose costs 1 to
-- 3 bytes per character: 200000 characters stay far below the limit, and
-- 2000 title characters (at most 8 bytes each, as a character can be part of
-- two lexemes of 4-byte characters, plus 5 per lexeme) always fit. Only
-- contrived text (hyphenated compounds of multibyte characters, which the
-- parser indexes whole and in parts) can pass the limit within 200000
-- characters; the body is then cut to a quarter until it fits, down to no
-- body at all.
CREATE FUNCTION search_put(
    p_project uuid, p_kind text, p_source uuid, p_track uuid, p_hypothesis uuid,
    p_attempt uuid, p_user uuid, p_service uuid, p_title text, p_body text,
    p_at timestamptz
) RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    body_chars integer := 200000;
BEGIN
    LOOP
        BEGIN
            INSERT INTO search_documents (project_id, kind, source_id, track_id,
                                          hypothesis_id, attempt_id, actor_user,
                                          actor_service, title, body, occurred_at)
            VALUES (p_project, p_kind, p_source, p_track, p_hypothesis, p_attempt, p_user,
                    p_service, left(coalesce(p_title, ''), 2000),
                    left(coalesce(p_body, ''), body_chars), p_at)
            ON CONFLICT (kind, source_id) DO UPDATE SET
                title = EXCLUDED.title, body = EXCLUDED.body, updated_at = now();
            RETURN;
        EXCEPTION WHEN program_limit_exceeded THEN
            IF body_chars = 0 THEN
                RAISE;
            END IF;
            body_chars := body_chars / 4;
        END;
    END LOOP;
END;
$$;

CREATE FUNCTION search_index_track(t tracks) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(t.project_id, 'track', t.id, t.id, NULL, NULL, t.created_by, NULL,
                      t.title, concat_ws(E'\n', t.slug, t.description), t.created_at)
$$;

-- A hypothesis document holds its current draft revision: the prose first,
-- then every other string of the document.
CREATE FUNCTION search_index_hypothesis(r hypothesis_revisions) RETURNS void
LANGUAGE sql AS $$
    SELECT search_put(h.project_id, 'hypothesis', h.id, NULL, h.id, NULL, h.created_by_user,
                      h.created_by_service, r.content ->> 'title',
                      concat_ws(E'\n', r.content ->> 'question', r.content ->> 'rationale',
                                r.content ->> 'intervention',
                                json_text(r.content - 'title' - 'schema_version' - 'track'
                                          - 'question' - 'rationale' - 'intervention')),
                      h.created_at)
    FROM hypotheses h WHERE h.id = r.hypothesis_id
$$;

-- The claimed sheet's report, the tester's observations and discrepancies,
-- and the evaluator's reason. Only an attempt's latest claimed sheet is
-- searchable: a new one replaces the report document of the previous one.
CREATE FUNCTION search_index_evidence(e evidence_records) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    hypothesis uuid;
    body text;
BEGIN
    SELECT hypothesis_id INTO hypothesis FROM attempts WHERE id = e.attempt_id;
    IF e.stage = 'agent' THEN
        DELETE FROM search_documents
        WHERE kind = 'report' AND attempt_id = e.attempt_id AND source_id <> e.id;
    END IF;
    IF e.stage = 'agent' AND e.content ? 'report' THEN
        PERFORM search_put(
            e.project_id, 'report', e.id, NULL, hypothesis, e.attempt_id, e.producer_user,
            e.producer_service, e.content #>> '{report,what_was_tried}',
            concat_ws(E'\n',
                e.content #>> '{report,configuration}', e.content #>> '{report,observations}',
                e.content #>> '{report,findings}', e.content #>> '{report,limitations}',
                e.content #>> '{report,next_question}', e.content ->> 'observations',
                e.content #>> '{report,body_markdown}'),
            e.created_at);
    ELSIF e.stage = 'tester' THEN
        body := concat_ws(E'\n', e.content ->> 'observations', (
            SELECT string_agg(d ->> 'description', E'\n')
            FROM jsonb_array_elements(coalesce(e.content -> 'discrepancies', '[]')) AS d));
        IF body ~ '\S' THEN
            PERFORM search_put(e.project_id, 'tester_observation', e.id, NULL, hypothesis,
                               e.attempt_id, e.producer_user, e.producer_service, '', body,
                               e.created_at);
        END IF;
    ELSIF e.stage = 'evaluator' AND e.content ? 'assessment' THEN
        PERFORM search_put(e.project_id, 'evaluator_reason', e.id, NULL, hypothesis,
                           e.attempt_id, e.producer_user, e.producer_service,
                           e.content #>> '{assessment,verdict}',
                           e.content #>> '{assessment,reason}', e.created_at);
    END IF;
END;
$$;

CREATE FUNCTION search_index_decision(d decisions) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(c.project_id, 'decision_reason', d.id, NULL, c.hypothesis_id,
                      c.attempt_id, d.actor_user_id, NULL, d.action, d.reason, d.decided_at)
    FROM review_cases c WHERE c.id = d.review_case_id
$$;

-- Every attempt has a document of its own, with no text, so that `#12.3`
-- finds an attempt that has no report, failure or comment yet.
CREATE FUNCTION search_index_attempt(a attempts) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(a.project_id, 'attempt', a.id, NULL, a.hypothesis_id, a.id,
                      a.claimed_by_user, a.claimed_by_service, '', '', a.claimed_at)
$$;

-- An attempt is also searchable by the reasons it failed.
CREATE FUNCTION search_index_failure(f attempt_failures) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(a.project_id, 'attempt', f.id, NULL, a.hypothesis_id, a.id, NULL, NULL,
                      f.code, f.reason, f.created_at)
    FROM attempts a WHERE a.id = f.attempt_id
$$;

CREATE FUNCTION search_index_comment(c comments) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(c.project_id, 'comment', c.id, NULL, c.hypothesis_id, c.attempt_id,
                      c.author_user, NULL, '', c.body_markdown, c.created_at)
$$;

-- Measurements of agent and tester evidence as rows. The authority is derived
-- from the stage that published the record, never read from the document.
CREATE TABLE measurements (
    id                 bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id         uuid NOT NULL REFERENCES projects (id),
    attempt_id         uuid NOT NULL REFERENCES attempts (id),
    evidence_id        uuid NOT NULL REFERENCES evidence_records (id),
    authority          text NOT NULL CHECK (authority IN ('agent_claim', 'tester_verified')),
    metric             text NOT NULL,
    split              text NOT NULL,
    dimensions         jsonb NOT NULL,
    dimension_keys     text[] NOT NULL,
    value              double precision,
    missing_reason     text,
    unit               text NOT NULL,
    direction          text NOT NULL,
    sample_count       numeric,
    control_value      double precision,
    uncertainty_method text,
    uncertainty_lower  double precision,
    uncertainty_upper  double precision,
    recorded_at        timestamptz NOT NULL
);
CREATE INDEX measurements_query_idx ON measurements (project_id, metric, split, authority, id);
CREATE INDEX measurements_attempt_idx ON measurements (attempt_id);
CREATE TRIGGER measurements_immutable BEFORE UPDATE OR DELETE ON measurements
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

CREATE FUNCTION measurements_from_evidence(e evidence_records) RETURNS void
LANGUAGE sql AS $$
    INSERT INTO measurements (project_id, attempt_id, evidence_id, authority, metric, split,
                              dimensions, dimension_keys, value, missing_reason, unit,
                              direction, sample_count, control_value, uncertainty_method,
                              uncertainty_lower, uncertainty_upper, recorded_at)
    SELECT e.project_id, e.attempt_id, e.id,
           CASE e.stage WHEN 'tester' THEN 'tester_verified' ELSE 'agent_claim' END,
           m ->> 'metric', m ->> 'split', coalesce(m -> 'dimensions', '{}'),
           ARRAY(SELECT k FROM jsonb_object_keys(coalesce(m -> 'dimensions', '{}')) AS k
                 ORDER BY k COLLATE "C"),
           (m ->> 'value')::double precision, m ->> 'missing_reason', m ->> 'unit',
           m ->> 'direction', (m ->> 'sample_count')::numeric,
           (m ->> 'control_value')::double precision, m #>> '{uncertainty,method}',
           (m #>> '{uncertainty,lower}')::double precision,
           (m #>> '{uncertainty,upper}')::double precision, e.created_at
    FROM jsonb_array_elements(coalesce(e.content -> 'measurements', '[]')) AS m
    WHERE e.stage = 'agent' OR (e.stage = 'tester' AND e.status = 'completed')
$$;

-- Triggers and backfill.

CREATE FUNCTION index_on_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    CASE TG_TABLE_NAME
        WHEN 'tracks' THEN PERFORM search_index_track(NEW);
        WHEN 'hypothesis_revisions' THEN PERFORM search_index_hypothesis(NEW);
        WHEN 'evidence_records' THEN
            PERFORM search_index_evidence(NEW);
            PERFORM measurements_from_evidence(NEW);
        WHEN 'decisions' THEN PERFORM search_index_decision(NEW);
        WHEN 'attempts' THEN PERFORM search_index_attempt(NEW);
        WHEN 'attempt_failures' THEN PERFORM search_index_failure(NEW);
        WHEN 'comments' THEN PERFORM search_index_comment(NEW);
    END CASE;
    RETURN NULL;
END;
$$;

CREATE TRIGGER tracks_indexed AFTER INSERT OR UPDATE OF title, description ON tracks
    FOR EACH ROW EXECUTE FUNCTION index_on_insert();
CREATE TRIGGER hypothesis_revisions_indexed AFTER INSERT ON hypothesis_revisions
    FOR EACH ROW EXECUTE FUNCTION index_on_insert();
CREATE TRIGGER evidence_records_indexed AFTER INSERT ON evidence_records
    FOR EACH ROW EXECUTE FUNCTION index_on_insert();
CREATE TRIGGER decisions_indexed AFTER INSERT ON decisions
    FOR EACH ROW EXECUTE FUNCTION index_on_insert();
CREATE TRIGGER attempts_indexed AFTER INSERT ON attempts
    FOR EACH ROW EXECUTE FUNCTION index_on_insert();
CREATE TRIGGER attempt_failures_indexed AFTER INSERT ON attempt_failures
    FOR EACH ROW EXECUTE FUNCTION index_on_insert();
CREATE TRIGGER comments_indexed AFTER INSERT OR UPDATE OF body_markdown ON comments
    FOR EACH ROW EXECUTE FUNCTION index_on_insert();

SELECT search_index_track(t) FROM tracks t ORDER BY t.created_at;
SELECT search_index_hypothesis(r)
FROM hypothesis_revisions r JOIN hypotheses h ON h.id = r.hypothesis_id AND h.revision = r.revision
ORDER BY h.created_at;
SELECT search_index_attempt(a) FROM attempts a ORDER BY a.claimed_at;
SELECT search_index_evidence(e), measurements_from_evidence(e)
FROM evidence_records e ORDER BY e.created_at, e.revision;
SELECT search_index_decision(d) FROM decisions d ORDER BY d.decided_at;
SELECT search_index_failure(f) FROM attempt_failures f ORDER BY f.created_at;

-- Mentions in the reports stored before this migration, which the API now
-- records at submission. The pattern is the API's (hypotheses/mentions.py).
-- Only mentions of the report's own project are resolved: a report submitted
-- by an agent can name no other project (the API resolves none for a service
-- account), and a person's access to another project when submitting is not
-- known any more, so such a mention stays plain text.
INSERT INTO mentions (source_type, source_id, target_id)
SELECT DISTINCT 'report', e.id, t.id
FROM evidence_records e
JOIN attempts a ON a.id = e.attempt_id
JOIN projects p ON p.id = e.project_id
CROSS JOIN LATERAL regexp_matches(
    json_text(e.content -> 'report'),
    '(?<![\w#/.-])(?:([a-z0-9][a-z0-9-]{0,62})#|#)([1-9][0-9]{0,8})\y', 'g') AS m
JOIN hypotheses t ON t.project_id = e.project_id AND t.number = m[2]::bigint
WHERE e.stage = 'agent' AND e.content ? 'report'
  AND (m[1] IS NULL OR m[1] = p.slug) AND t.id <> a.hypothesis_id
ON CONFLICT DO NOTHING;
