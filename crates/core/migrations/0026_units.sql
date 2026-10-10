-- SPDX-License-Identifier: AGPL-3.0-only
-- Hypotheses become units: a unit of work is what a track's plan lists. The
-- tables, columns, constraints, indexes, triggers and functions are renamed,
-- and so are the stored names the code reads back: the search document kind,
-- the mention source type, the import entry kind, the audit actions, subject
-- type and state keys, and the `hypothesis` keys of job specs, concern front
-- matter, relations and science revisions. Every row keeps its id and links.

-- Tables and columns.
ALTER TABLE hypotheses RENAME TO units;
ALTER TABLE hypothesis_revisions RENAME TO unit_revisions;
ALTER TABLE hypothesis_relations RENAME TO unit_relations;
ALTER TABLE projects RENAME COLUMN next_hypothesis_number TO next_unit_number;
ALTER TABLE unit_revisions RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE unit_relations RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE attempts RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE attempts RENAME COLUMN hypothesis_revision TO unit_revision;
ALTER TABLE comments RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE comparisons RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE concerns RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE plan_alignments RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE plan_units RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE plan_units RENAME COLUMN hypothesis_revision TO unit_revision;
ALTER TABLE review_cases RENAME COLUMN hypothesis_id TO unit_id;
ALTER TABLE search_documents RENAME COLUMN hypothesis_id TO unit_id;

-- Constraints (with the indexes behind them), then the other indexes.
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN
        SELECT conrelid::regclass AS tbl, conname FROM pg_constraint
        WHERE connamespace = 'public'::regnamespace AND conname LIKE '%hypothes%'
    LOOP
        EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I', r.tbl, r.conname,
                       replace(replace(r.conname, 'hypotheses', 'units'), 'hypothesis', 'unit'));
    END LOOP;
    FOR r IN
        SELECT c.relname FROM pg_class c
        WHERE c.relnamespace = 'public'::regnamespace AND c.relkind = 'i'
          AND c.relname LIKE '%hypothes%'
    LOOP
        EXECUTE format('ALTER INDEX %I RENAME TO %I', r.relname,
                       replace(replace(r.relname, 'hypotheses', 'units'), 'hypothesis', 'unit'));
    END LOOP;
END;
$$;

ALTER TRIGGER hypothesis_revisions_immutable ON unit_revisions RENAME TO unit_revisions_immutable;
ALTER TRIGGER hypothesis_revisions_indexed ON unit_revisions RENAME TO unit_revisions_indexed;

-- Functions whose bodies name the renamed tables and columns.
DROP FUNCTION search_index_hypothesis(unit_revisions);
DROP FUNCTION search_put(uuid, text, uuid, uuid, uuid, uuid, uuid, uuid, text, text, timestamptz);

CREATE FUNCTION search_put(p_project uuid, p_kind text, p_source uuid, p_track uuid,
                           p_unit uuid, p_attempt uuid, p_user uuid, p_service uuid,
                           p_title text, p_body text, p_at timestamptz)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    body_chars integer := 200000;
BEGIN
    LOOP
        BEGIN
            INSERT INTO search_documents (project_id, kind, source_id, track_id,
                                          unit_id, attempt_id, actor_user,
                                          actor_service, title, body, occurred_at)
            VALUES (p_project, p_kind, p_source, p_track, p_unit, p_attempt, p_user,
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

CREATE FUNCTION search_index_unit(r unit_revisions) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(u.project_id, 'unit', u.id, NULL, u.id, NULL, u.created_by_user,
                      u.created_by_service, r.content ->> 'title',
                      concat_ws(E'\n', r.content ->> 'question', r.content ->> 'rationale',
                                r.content ->> 'intervention',
                                json_text(r.content - 'title' - 'schema_version' - 'track'
                                          - 'question' - 'rationale' - 'intervention')),
                      u.created_at)
    FROM units u WHERE u.id = r.unit_id
$$;

CREATE OR REPLACE FUNCTION index_on_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    CASE TG_TABLE_NAME
        WHEN 'tracks' THEN PERFORM search_index_track(NEW);
        WHEN 'unit_revisions' THEN PERFORM search_index_unit(NEW);
        WHEN 'phase_outputs' THEN
            PERFORM search_index_evidence(NEW);
            PERFORM measurements_from_evidence(NEW);
            PERFORM comparisons_from_evidence(NEW);
        WHEN 'decisions' THEN PERFORM search_index_decision(NEW);
        WHEN 'attempts' THEN PERFORM search_index_attempt(NEW);
        WHEN 'attempt_failures' THEN PERFORM search_index_failure(NEW);
        WHEN 'comments' THEN PERFORM search_index_comment(NEW);
    END CASE;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION search_index_attempt(a attempts) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(a.project_id, 'attempt', a.id, NULL, a.unit_id, a.id,
                      a.claimed_by_user, a.claimed_by_service, '', '', a.claimed_at)
$$;

CREATE OR REPLACE FUNCTION search_index_comment(c comments) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(c.project_id, 'comment', c.id, NULL, c.unit_id, c.attempt_id,
                      c.author_user, NULL, '', c.body_markdown, c.created_at)
$$;

CREATE OR REPLACE FUNCTION search_index_decision(d decisions) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(c.project_id, 'decision_reason', d.id, NULL, c.unit_id,
                      c.attempt_id, d.actor_user_id, d.actor_service_id, d.action, d.reason,
                      d.decided_at)
    FROM review_cases c WHERE c.id = d.review_case_id AND c.unit_id IS NOT NULL
$$;

CREATE OR REPLACE FUNCTION search_index_failure(f attempt_failures) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(a.project_id, 'attempt', f.id, NULL, a.unit_id, a.id, NULL, NULL,
                      f.code, f.reason, f.created_at)
    FROM attempts a WHERE a.id = f.attempt_id
$$;

CREATE OR REPLACE FUNCTION search_index_evidence(e phase_outputs) RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    unit uuid;
    body text;
BEGIN
    IF e.stage NOT IN ('verification', 'writeup') THEN
        RETURN;
    END IF;
    SELECT unit_id INTO unit FROM attempts WHERE id = e.attempt_id;
    IF e.stage = 'writeup' THEN
        PERFORM search_put(e.project_id, 'writeup', e.id, NULL, unit, e.attempt_id,
                           e.producer_user, e.producer_service,
                           coalesce(e.front_matter ->> 'summary', ''), e.body, e.created_at);
        RETURN;
    END IF;
    body := concat_ws(E'\n', e.front_matter ->> 'reason', nullif(e.body, ''), (
        SELECT string_agg(d ->> 'description', E'\n')
        FROM jsonb_array_elements(
            CASE WHEN jsonb_typeof(e.front_matter -> 'discrepancies') = 'array'
                 THEN e.front_matter -> 'discrepancies' ELSE '[]'::jsonb END) AS d));
    IF body ~ '\S' OR e.front_matter ? 'verdict' THEN
        PERFORM search_put(e.project_id, 'verification', e.id, NULL, unit, e.attempt_id,
                           e.producer_user, e.producer_service,
                           coalesce(e.front_matter ->> 'verdict', ''), body, e.created_at);
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION comparisons_from_evidence(e phase_outputs) RETURNS void LANGUAGE sql AS $$
    INSERT INTO comparisons (project_id, track_id, unit_id, attempt_id, evidence_id,
                             metric, split, dimensions, dimension_keys, value, source,
                             reference_value, reference_label, reference_kind, reference_ref,
                             verdict, policy_revision, recorded_at)
    SELECT e.project_id, a.track_id, a.unit_id, e.attempt_id, e.id,
           c ->> 'metric', c ->> 'split', coalesce(c -> 'dimensions', '{}'),
           ARRAY(SELECT k FROM jsonb_object_keys(coalesce(c -> 'dimensions', '{}')) AS k
                 ORDER BY k COLLATE "C"),
           (c ->> 'value')::double precision, c ->> 'source',
           (c #>> '{reference,value}')::double precision, c #>> '{reference,label}',
           c #>> '{reference,kind}', c #>> '{reference,ref}',
           e.front_matter ->> 'verdict', e.front_matter ->> 'policy_revision', e.created_at
    FROM attempts a,
         jsonb_array_elements(coalesce(e.front_matter -> 'comparisons', '[]')) AS c
    WHERE a.id = e.attempt_id AND e.stage = 'verification' AND e.status = 'completed'
$$;

CREATE OR REPLACE FUNCTION comments_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'comments are never deleted';
    END IF;
    IF (NEW.id, NEW.project_id, NEW.unit_id, NEW.attempt_id, NEW.author_user,
        NEW.created_at)
       IS DISTINCT FROM
       (OLD.id, OLD.project_id, OLD.unit_id, OLD.attempt_id, OLD.author_user,
        OLD.created_at) THEN
        RAISE EXCEPTION 'only a comment''s body changes';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'an edit is the next revision of the comment';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION concerns_close_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'concerns are never deleted';
    END IF;
    IF OLD.state <> 'open' THEN
        RAISE EXCEPTION 'a closed concern is immutable';
    END IF;
    IF (NEW.id, NEW.project_id, NEW.track_id, NEW.kind, NEW.unit_id, NEW.attempt_id,
        NEW.front_matter, NEW.body, NEW.sha256, NEW.raised_by_user, NEW.raised_by_service,
        NEW.via_channel, NEW.via_client, NEW.raised_at)
       IS DISTINCT FROM
       (OLD.id, OLD.project_id, OLD.track_id, OLD.kind, OLD.unit_id, OLD.attempt_id,
        OLD.front_matter, OLD.body, OLD.sha256, OLD.raised_by_user, OLD.raised_by_service,
        OLD.via_channel, OLD.via_client, OLD.raised_at) THEN
        RAISE EXCEPTION 'a concern''s document is immutable';
    END IF;
    RETURN NEW;
END;
$$;

-- Stored names. The append-only tables are rewritten with their guards off.
ALTER TABLE audit_events DISABLE TRIGGER audit_events_no_update;
ALTER TABLE unit_revisions DISABLE TRIGGER unit_revisions_immutable;
ALTER TABLE config_revisions DISABLE TRIGGER config_revisions_immutable;
ALTER TABLE import_entries DISABLE TRIGGER import_entries_immutable;
ALTER TABLE jobs DISABLE TRIGGER jobs_frozen;
ALTER TABLE concerns DISABLE TRIGGER concerns_close_only;

-- A JSON object with each key that names hypotheses renamed.
CREATE FUNCTION pg_temp.unit_keys(value jsonb) RETURNS jsonb LANGUAGE sql AS $$
    SELECT CASE WHEN jsonb_typeof(value) = 'object' THEN coalesce((
        SELECT jsonb_object_agg(replace(replace(key, 'hypotheses', 'units'), 'hypothesis', 'unit'),
                                item)
        FROM jsonb_each(value) AS e(key, item)), '{}'::jsonb)
    ELSE value END
$$;
-- A JSON value with every key exactly `hypothesis` or `hypotheses` renamed,
-- at any depth.
CREATE FUNCTION pg_temp.unit_keys_deep(value jsonb) RETURNS jsonb LANGUAGE sql AS $$
    SELECT CASE jsonb_typeof(value)
        WHEN 'object' THEN coalesce((
            SELECT jsonb_object_agg(CASE key WHEN 'hypothesis' THEN 'unit'
                                             WHEN 'hypotheses' THEN 'units' ELSE key END,
                                    pg_temp.unit_keys_deep(item))
            FROM jsonb_each(value) AS e(key, item)), '{}'::jsonb)
        WHEN 'array' THEN coalesce((
            SELECT jsonb_agg(pg_temp.unit_keys_deep(item) ORDER BY n)
            FROM jsonb_array_elements(value) WITH ORDINALITY AS e(item, n)), '[]'::jsonb)
        ELSE value END
$$;
-- Relations name their target `unit`.
CREATE FUNCTION pg_temp.unit_relations(value jsonb) RETURNS jsonb LANGUAGE sql AS $$
    SELECT CASE WHEN jsonb_typeof(value -> 'relations') = 'array'
        THEN jsonb_set(value, '{relations}', coalesce((
            SELECT jsonb_agg(CASE WHEN jsonb_typeof(item) = 'object' AND item ? 'hypothesis'
                                  THEN (item - 'hypothesis')
                                       || jsonb_build_object('unit', item -> 'hypothesis')
                                  ELSE item END ORDER BY n)
            FROM jsonb_array_elements(value -> 'relations') WITH ORDINALITY AS e(item, n)),
            '[]'::jsonb))
        ELSE value END
$$;

UPDATE audit_events
SET action = replace(action, 'hypothesis', 'unit'),
    subject_type = CASE subject_type WHEN 'hypothesis' THEN 'unit' ELSE subject_type END,
    prior_state = pg_temp.unit_keys(prior_state),
    new_state = pg_temp.unit_keys(new_state)
WHERE action LIKE '%hypothesis%' OR subject_type = 'hypothesis'
   OR prior_state::text LIKE '%hypothes%' OR new_state::text LIKE '%hypothes%';

UPDATE unit_revisions SET content = pg_temp.unit_relations(content)
WHERE content -> 'relations' @? '$[*].hypothesis';
UPDATE plan_units SET fields = pg_temp.unit_relations(fields)
WHERE fields -> 'relations' @? '$[*].hypothesis';

UPDATE config_revisions
SET content = (content - 'hypothesis_fields')
              || jsonb_build_object('unit_fields', content -> 'hypothesis_fields')
WHERE kind = 'science' AND content ? 'hypothesis_fields';
-- Dashboard views name unit fields `unit.<field>`.
UPDATE config_revisions
SET content = regexp_replace(content::text, '"hypothesis\.([a-z][a-z0-9_]{0,63})"', '"unit.\1"',
                             'g')::jsonb
WHERE kind = 'dashboard' AND content::text ~ '"hypothesis\.[a-z]';

UPDATE jobs SET spec = (spec - 'hypothesis') || jsonb_build_object('unit', spec -> 'hypothesis')
WHERE spec ? 'hypothesis';
-- The digest stays that of the document as it was raised.
UPDATE concerns
SET front_matter = (front_matter - 'hypothesis')
                   || jsonb_build_object('unit', front_matter -> 'hypothesis')
WHERE front_matter ? 'hypothesis';

UPDATE units SET imported = pg_temp.unit_keys_deep(imported)
WHERE imported::text LIKE '%"hypothes%';
UPDATE attempts SET imported = pg_temp.unit_keys_deep(imported)
WHERE imported::text LIKE '%"hypothes%';

ALTER TABLE search_documents DROP CONSTRAINT search_documents_kind_check;
UPDATE search_documents SET kind = 'unit' WHERE kind = 'hypothesis';
ALTER TABLE search_documents ADD CONSTRAINT search_documents_kind_check CHECK (kind IN (
    'track', 'unit', 'attempt', 'report', 'verification', 'writeup', 'decision_reason',
    'comment'));

ALTER TABLE mentions DROP CONSTRAINT mentions_source_type_check;
UPDATE mentions SET source_type = 'unit' WHERE source_type = 'hypothesis';
ALTER TABLE mentions ADD CONSTRAINT mentions_source_type_check
    CHECK (source_type IN ('unit', 'attempt', 'comment', 'report'));

ALTER TABLE import_entries DROP CONSTRAINT import_entries_kind_check;
UPDATE import_entries SET kind = 'unit' WHERE kind = 'hypothesis';
ALTER TABLE import_entries ADD CONSTRAINT import_entries_kind_check
    CHECK (kind IN ('project', 'track', 'policy', 'unit'));

ALTER TABLE audit_events ENABLE TRIGGER audit_events_no_update;
ALTER TABLE unit_revisions ENABLE TRIGGER unit_revisions_immutable;
ALTER TABLE config_revisions ENABLE TRIGGER config_revisions_immutable;
ALTER TABLE import_entries ENABLE TRIGGER import_entries_immutable;
ALTER TABLE jobs ENABLE TRIGGER jobs_frozen;
ALTER TABLE concerns ENABLE TRIGGER concerns_close_only;

DROP FUNCTION pg_temp.unit_keys(jsonb);
DROP FUNCTION pg_temp.unit_keys_deep(jsonb);
DROP FUNCTION pg_temp.unit_relations(jsonb);
