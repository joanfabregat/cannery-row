-- SPDX-License-Identifier: AGPL-3.0-only
-- Questions, answers and steering notes between a unit's performer and the
-- project's researchers, and the performer's transcript.
--
-- A performer (an agent holding an attempt in agent mode, or anyone holding
-- a job) asks a question about its unit. A blocking question stops the
-- performer's lease clock: the attempt waits in `waiting_on_human`, a job
-- keeps its state, both with a `lease_pauses` row, and neither lease nor deadline
-- expires until a researcher answers or escalates it. A non-blocking one
-- names the default the performer proceeds on. A researcher answers a
-- question, escalates it into a concern about the track's plan, or posts a
-- steering note to a running attempt unasked. Messages are written once;
-- a question only closes, and an answer or steering note is only
-- acknowledged by the performer it was meant for.

-- Waiting on a person: the attempt keeps its lease token, and its clock
-- stops at its `lease_pauses` row's `paused_at`.
ALTER TABLE attempts DROP CONSTRAINT attempts_state_check;
ALTER TABLE attempts ADD CONSTRAINT attempts_state_check CHECK (state IN (
    'claimed', 'running', 'waiting_on_human', 'verifying', 'verified', 'failed', 'cancelled',
    'unreviewed'));
DROP INDEX attempts_one_open_idx;
CREATE UNIQUE INDEX attempts_one_open_idx ON attempts (unit_id)
    WHERE state IN ('claimed', 'running', 'waiting_on_human', 'verifying');
DO $$
DECLARE
    found text;
BEGIN
    SELECT conname INTO found FROM pg_constraint
    WHERE conrelid = 'attempts'::regclass AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%lease_token_hash IS NOT NULL%'
      AND pg_get_constraintdef(oid) LIKE '%state%';
    EXECUTE format('ALTER TABLE attempts DROP CONSTRAINT %I', found);
END;
$$;
ALTER TABLE attempts ADD CONSTRAINT attempts_lease_check
    CHECK ((state IN ('claimed', 'running', 'waiting_on_human')) = (lease_token_hash IS NOT NULL));

-- A lease whose clock is stopped: one row per paused attempt lease (no
-- `job_id`) or job lease, from when it stopped until it runs again.
CREATE TABLE lease_pauses (
    attempt_id uuid NOT NULL REFERENCES attempts(id),
    job_id uuid REFERENCES jobs(id),
    paused_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX lease_pauses_attempt_idx ON lease_pauses (attempt_id) WHERE job_id IS NULL;
CREATE UNIQUE INDEX lease_pauses_job_idx ON lease_pauses (job_id) WHERE job_id IS NOT NULL;

-- How long a blocking question may wait, and how large a transcript grows.
ALTER TABLE project_limits
    ADD COLUMN question_wait_seconds integer NOT NULL DEFAULT 86400
        CHECK (question_wait_seconds BETWEEN 60 AND 2592000),
    ADD COLUMN transcript_max_bytes bigint NOT NULL DEFAULT 67108864
        CHECK (transcript_max_bytes BETWEEN 65536 AND 1073741824);

CREATE TABLE messages (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      uuid NOT NULL REFERENCES projects (id),
    unit_id         uuid NOT NULL REFERENCES units (id),
    attempt_id      uuid NOT NULL REFERENCES attempts (id),
    -- The job whose lease a question was asked under; none for an attempt's.
    job_id          uuid REFERENCES jobs (id),
    kind            text NOT NULL CHECK (kind IN ('question', 'answer', 'steer')),
    blocking        boolean,
    -- What the performer of a non-blocking question proceeds on meanwhile.
    default_text    text CHECK (default_text IS NULL
                                OR (default_text ~ '\S' AND octet_length(default_text) <= 4000)),
    body            text NOT NULL CHECK (body ~ '\S' AND octet_length(body) <= 16384),
    sha256          text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    -- The question an answer answers.
    question_id     uuid REFERENCES messages (id),
    author_user     uuid REFERENCES users (id),
    author_service  uuid REFERENCES service_accounts (id),
    via_channel     text NOT NULL CHECK (via_channel IN ('ui', 'api', 'mcp', 'cli')),
    via_client      text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- A question's state: open, then answered or escalated, once.
    state           text CHECK (state IN ('open', 'answered', 'escalated')),
    closed_at       timestamptz,
    -- The concern an escalated question became.
    concern_id      uuid REFERENCES concerns (id),
    -- When its attempt or job was released because this blocking question
    -- went unanswered; the question stays open for the next attempt.
    released_at     timestamptz,
    -- When the performer acknowledged this answer or steering note.
    acknowledged_at timestamptz,
    CHECK (num_nonnulls(author_user, author_service) = 1),
    CHECK ((kind = 'question') = (blocking IS NOT NULL)),
    CHECK ((kind = 'question') = (state IS NOT NULL)),
    CHECK ((kind = 'answer') = (question_id IS NOT NULL)),
    CHECK (kind = 'question' OR default_text IS NULL),
    CHECK (kind <> 'question' OR blocking OR default_text IS NOT NULL),
    CHECK (kind <> 'question' OR (state = 'open') = (closed_at IS NULL)),
    CHECK (kind <> 'question' OR (state = 'escalated') = (concern_id IS NOT NULL)),
    CHECK (kind = 'question' OR (concern_id IS NULL AND released_at IS NULL)),
    CHECK (kind <> 'question' OR acknowledged_at IS NULL),
    CHECK (kind <> 'steer' OR job_id IS NULL),
    CHECK (kind <> 'steer' OR author_user IS NOT NULL),
    CHECK (kind <> 'answer' OR author_user IS NOT NULL)
);
CREATE UNIQUE INDEX messages_one_answer_idx ON messages (question_id) WHERE kind = 'answer';
CREATE INDEX messages_attempt_idx ON messages (attempt_id, created_at, id);
CREATE INDEX messages_unit_idx ON messages (unit_id, created_at, id);
CREATE INDEX messages_project_idx ON messages (project_id, created_at, id) WHERE kind = 'question';
CREATE INDEX messages_waiting_idx ON messages (created_at)
    WHERE kind = 'question' AND blocking AND state = 'open' AND released_at IS NULL;

-- A message is written once; a question only closes, once, and is released
-- once; an answer or a steering note is only acknowledged, once.
CREATE FUNCTION messages_close_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messages are never deleted';
    END IF;
    IF (NEW.id, NEW.project_id, NEW.unit_id, NEW.attempt_id, NEW.job_id, NEW.kind,
        NEW.blocking, NEW.default_text, NEW.body, NEW.sha256, NEW.question_id,
        NEW.author_user, NEW.author_service, NEW.via_channel, NEW.via_client, NEW.created_at)
       IS DISTINCT FROM
       (OLD.id, OLD.project_id, OLD.unit_id, OLD.attempt_id, OLD.job_id, OLD.kind,
        OLD.blocking, OLD.default_text, OLD.body, OLD.sha256, OLD.question_id,
        OLD.author_user, OLD.author_service, OLD.via_channel, OLD.via_client, OLD.created_at) THEN
        RAISE EXCEPTION 'a message is immutable';
    END IF;
    IF OLD.state IS DISTINCT FROM NEW.state AND OLD.state <> 'open' THEN
        RAISE EXCEPTION 'a closed question is immutable';
    END IF;
    IF OLD.released_at IS NOT NULL AND NEW.released_at IS DISTINCT FROM OLD.released_at THEN
        RAISE EXCEPTION 'a question is released once';
    END IF;
    IF OLD.acknowledged_at IS NOT NULL
       AND NEW.acknowledged_at IS DISTINCT FROM OLD.acknowledged_at THEN
        RAISE EXCEPTION 'a message is acknowledged once';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER messages_close_only BEFORE UPDATE OR DELETE ON messages
    FOR EACH ROW EXECUTE FUNCTION messages_close_only();

-- A transcript is appended in chunks while its attempt runs: each chunk is
-- an object of JSON Lines, verified and immutable. Submitting the attempt
-- seals it into one `transcript` artifact.
CREATE TABLE transcript_chunks (
    attempt_id     uuid NOT NULL REFERENCES attempts (id),
    sequence       integer NOT NULL CHECK (sequence > 0),
    -- The index of the chunk's first event in the transcript, from 0.
    first_event    integer NOT NULL CHECK (first_event >= 0),
    events         integer NOT NULL CHECK (events > 0),
    backend        text NOT NULL,
    bucket         text NOT NULL,
    key            text NOT NULL,
    size_bytes     bigint NOT NULL CHECK (size_bytes > 0),
    sha256         text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    author_user    uuid REFERENCES users (id),
    author_service uuid REFERENCES service_accounts (id),
    via_channel    text NOT NULL,
    via_client     text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (attempt_id, sequence),
    UNIQUE (backend, bucket, key),
    CHECK (num_nonnulls(author_user, author_service) = 1)
);
CREATE TRIGGER transcript_chunks_immutable BEFORE UPDATE OR DELETE ON transcript_chunks
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Search: every message is one document, titled by its kind.
ALTER TABLE search_documents DROP CONSTRAINT search_documents_kind_check;
ALTER TABLE search_documents ADD CONSTRAINT search_documents_kind_check CHECK (kind IN (
    'track', 'unit', 'attempt', 'report', 'verification', 'writeup', 'decision_reason',
    'comment', 'message'));

CREATE FUNCTION search_index_message(m messages) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(m.project_id, 'message', m.id, NULL, m.unit_id, m.attempt_id,
                      m.author_user, m.author_service,
                      CASE m.kind WHEN 'question' THEN 'Question'
                                  WHEN 'answer' THEN 'Answer'
                                  ELSE 'Steering note' END,
                      concat_ws(E'\n', m.body, m.default_text), m.created_at);
$$;
CREATE FUNCTION messages_index() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM search_index_message(NEW);
    RETURN NEW;
END;
$$;
CREATE TRIGGER messages_indexed AFTER INSERT ON messages
    FOR EACH ROW EXECUTE FUNCTION messages_index();
