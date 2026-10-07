-- Runner-driven experiments (#48). A track runs its experiments in one of two
-- modes: `agent` (outside agents claim, as before) or `workflow` (a Cannery Row
-- runner claims and runs the track's experiment workflow). See docs/spec.md.

-- Registered experiment step manifests (role `experiment`), per project, each
-- revision immutable, exactly like producer manifests.
CREATE TABLE experiment_manifests (
    project_id uuid NOT NULL REFERENCES projects (id),
    name       text NOT NULL CHECK (name ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    revision   integer NOT NULL CHECK (revision > 0),
    content    jsonb NOT NULL,
    created_by uuid NOT NULL REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, name, revision)
);
CREATE TRIGGER experiment_manifests_immutable BEFORE UPDATE OR DELETE ON experiment_manifests
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Every existing track keeps today's behaviour: `agent`. A `workflow` track
-- names its workflow, `{"steps": [{"name", "revision"}, ...]}`, of registered
-- experiment manifests; an `agent` track has none.
ALTER TABLE tracks ADD COLUMN mode text NOT NULL DEFAULT 'agent'
    CHECK (mode IN ('agent', 'workflow'));
ALTER TABLE tracks ADD COLUMN workflow jsonb CHECK (jsonb_typeof(workflow) = 'object');
ALTER TABLE tracks ADD CONSTRAINT tracks_workflow_mode_check
    CHECK ((mode = 'workflow') = (workflow IS NOT NULL));

-- A runner-driven attempt pins the workflow it runs at claim, like the
-- producer; NULL for an attempt an outside agent runs (every existing one).
ALTER TABLE attempts ADD COLUMN workflow jsonb CHECK (jsonb_typeof(workflow) = 'object');

-- An infrastructure failure of a runner-driven attempt requeues its
-- hypothesis automatically (no review case) while the science revision's
-- `max_auto_retries` allows; such failures are marked, and counted.
ALTER TABLE attempt_failures ADD COLUMN requeued boolean NOT NULL DEFAULT false;

-- The identity of a Cannery Row runner of the experiment kind. Only an
-- experimenter claims workflow hypotheses, reports a run's failure codes
-- and reads a predecessor's artifacts under a lease; an agent cannot.
ALTER TABLE service_accounts DROP CONSTRAINT service_accounts_kind_check;
ALTER TABLE service_accounts ADD CONSTRAINT service_accounts_kind_check
    CHECK (kind IN ('agent', 'experimenter', 'tester', 'evaluator'));

-- A runner-driven attempt's deadline, set at claim from its pinned steps; the
-- sweep fails an attempt past it (deadline_exceeded). NULL in agent mode.
ALTER TABLE attempts ADD COLUMN deadline timestamptz;
ALTER TABLE attempts ADD CONSTRAINT attempts_deadline_workflow_check
    CHECK (deadline IS NULL OR workflow IS NOT NULL);
