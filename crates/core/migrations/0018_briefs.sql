-- SPDX-License-Identifier: AGPL-3.0-only
-- Project briefs: the context every performer of a project's work is given,
-- its goal, domain, constraints, resources and conventions. A brief is one
-- Markdown document with YAML front matter (`brief.schema.json`), written by
-- a researcher. Each revision is immutable and attributed; the latest one is
-- the project's brief.

CREATE TABLE briefs (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id   uuid NOT NULL REFERENCES projects (id),
    revision     integer NOT NULL CHECK (revision > 0),
    -- The document as submitted, its parsed front matter and its body.
    document     text NOT NULL CHECK (octet_length(document) <= 262144),
    front_matter jsonb NOT NULL CHECK (jsonb_typeof(front_matter) = 'object'
                                       AND front_matter ->> 'title' ~ '\S'
                                       AND front_matter ->> 'goal' ~ '\S'),
    body         text NOT NULL,
    -- SHA-256 of the document's UTF-8 bytes.
    sha256       text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    created_by   uuid NOT NULL REFERENCES users (id),
    via_channel  text NOT NULL CHECK (via_channel IN ('ui', 'api', 'mcp', 'cli')),
    via_client   text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, revision)
);
CREATE TRIGGER briefs_immutable BEFORE UPDATE OR DELETE ON briefs
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- An attempt pins the brief revision current at its claim; NULL when the
-- project had no brief yet.
ALTER TABLE attempts ADD COLUMN brief_revision integer;
ALTER TABLE attempts ADD CONSTRAINT attempts_brief_fkey
    FOREIGN KEY (project_id, brief_revision) REFERENCES briefs (project_id, revision);
