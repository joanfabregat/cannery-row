-- An imported attempt's report (#59): the Markdown narrative of a historical
-- run, written after the fact from its sources (`retrospective`) and loaded
-- by `cannery import` from the bundle's reports/ directory. It is imported
-- history, not a claimed result sheet: it is never an evidence record, never
-- listed among the reports of the project, and holds no measurement. Only an
-- imported attempt has one, at most one, and it is immutable. See
-- docs/import.md.

-- The composite key lets a report reference an attempt together with its
-- origin, so only an imported attempt can have one.
ALTER TABLE attempts ADD CONSTRAINT attempts_id_origin_key UNIQUE (id, origin);

CREATE TABLE imported_reports (
    attempt_id    uuid PRIMARY KEY,
    origin        text NOT NULL DEFAULT 'imported' CHECK (origin = 'imported'),
    kind          text NOT NULL CHECK (kind IN ('retrospective')),
    author        text NOT NULL CHECK (author ~ '\S'),
    -- When it was written, as the bundle gives it: a day (`written_on`) or an
    -- instant (`written_at`), exactly one, never a time the source does not state.
    written_on    date,
    written_at    timestamptz,
    body_markdown text NOT NULL CHECK (body_markdown ~ '\S' AND octet_length(body_markdown) <= 262144),
    sha256        text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    -- The report's path in the bundle (reports/...).
    source_ref    text NOT NULL CHECK (source_ref ~ '\S'),
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(written_on, written_at) = 1),
    FOREIGN KEY (attempt_id, origin) REFERENCES attempts (id, origin)
);
CREATE TRIGGER imported_reports_immutable BEFORE UPDATE OR DELETE ON imported_reports
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
